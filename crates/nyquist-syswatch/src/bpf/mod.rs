mod sysctl_skel {
    include!(concat!(env!("OUT_DIR"), "/syswatch_sysctl.bpf.rs"));
}
mod ethtool_skel {
    include!(concat!(env!("OUT_DIR"), "/syswatch_ethtool.bpf.rs"));
}
mod rtnetlink_skel {
    include!(concat!(env!("OUT_DIR"), "/syswatch_rtnetlink.bpf.rs"));
}

use std::mem::MaybeUninit;
use std::sync::mpsc::SyncSender;
use std::time::Duration;
use libbpf_rs::{RingBuffer, RingBufferBuilder};
use libbpf_rs::skel::{OpenSkel, Skel, SkelBuilder};
use crate::event::SwEvent;

pub struct BpfState {
    _sysctl:    Option<Box<sysctl_skel::ModSkel<'static>>>,
    _ethtool:   Option<Box<ethtool_skel::ModSkel<'static>>>,
    _rtnetlink: Option<Box<rtnetlink_skel::ModSkel<'static>>>,
    rb:         RingBuffer<'static>,
}

unsafe impl Send for BpfState {}

pub struct LoadReport {
    pub sysctl:    Result<(), String>,
    pub ethtool:   Result<(), String>,
    pub rtnetlink: Result<(), String>,
}

impl BpfState {
    /// Load and attach all three BPF programs independently.
    /// Returns Err only if ALL three fail.
    pub fn load(tx: SyncSender<SwEvent>) -> anyhow::Result<(Self, LoadReport)> {
        // Phase 1: load and attach each skeleton independently.
        // Skeletons are transmuted to 'static and stored in Boxes so their heap
        // addresses are stable — we derive raw map pointers from them in phase 2.
        let (sc, sc_r) = load_sysctl();
        let (eth, eth_r) = load_ethtool();
        let (nl, nl_r) = load_rtnetlink();

        if sc.is_none() && eth.is_none() && nl.is_none() {
            return Err(anyhow::anyhow!(
                "all BPF programs failed — sysctl: {} | ethtool: {} | rtnetlink: {}",
                err_str(&sc_r), err_str(&eth_r), err_str(&nl_r),
            ));
        }

        // Phase 2: register ring buffer maps.
        // SAFETY: each skel is heap-allocated (Box) and will be stored in BpfState
        // for the lifetime of the RingBuffer. The &'static map references are
        // derived from stable heap addresses; moving the Box does not invalidate them.
        let mut builder = RingBufferBuilder::new();

        if let Some(ref skel) = sc {
            let tx2 = tx.clone();
            let map: &'static _ = unsafe { &*(&skel.maps.events as *const _) };
            builder.add(map, move |d: &[u8]| { forward(d, &tx2); 0 })?;
        }
        if let Some(ref skel) = eth {
            let tx2 = tx.clone();
            let map: &'static _ = unsafe { &*(&skel.maps.events as *const _) };
            builder.add(map, move |d: &[u8]| { forward(d, &tx2); 0 })?;
        }
        if let Some(ref skel) = nl {
            let tx2 = tx;
            let map: &'static _ = unsafe { &*(&skel.maps.events as *const _) };
            builder.add(map, move |d: &[u8]| { forward(d, &tx2); 0 })?;
        }

        let rb: RingBuffer<'static> = unsafe { std::mem::transmute(builder.build()?) };
        Ok((
            BpfState { _sysctl: sc, _ethtool: eth, _rtnetlink: nl, rb },
            LoadReport { sysctl: sc_r, ethtool: eth_r, rtnetlink: nl_r },
        ))
    }

    pub fn poll(&self, timeout: Duration) -> anyhow::Result<()> {
        self.rb.poll(timeout)?;
        Ok(())
    }
}

fn forward(data: &[u8], tx: &SyncSender<SwEvent>) {
    if let Some(e) = SwEvent::from_bytes(data) {
        let _ = tx.try_send(*e);
    }
}

fn err_str(r: &Result<(), String>) -> &str {
    match r { Ok(()) => "ok", Err(e) => e.as_str() }
}

fn load_sysctl() -> (Option<Box<sysctl_skel::ModSkel<'static>>>, Result<(), String>) {
    let mut obj = MaybeUninit::uninit();
    let open = match sysctl_skel::ModSkelBuilder::default().open(&mut obj) {
        Err(e) => return (None, Err(e.to_string())),
        Ok(o)  => o,
    };
    let mut loaded = match open.load() {
        Err(e) => return (None, Err(e.to_string())),
        Ok(l)  => l,
    };
    if let Err(e) = loaded.attach() {
        return (None, Err(e.to_string()));
    }
    let skel: Box<sysctl_skel::ModSkel<'static>> =
        unsafe { std::mem::transmute(Box::new(loaded)) };
    (Some(skel), Ok(()))
}

fn load_ethtool() -> (Option<Box<ethtool_skel::ModSkel<'static>>>, Result<(), String>) {
    let mut obj = MaybeUninit::uninit();
    let open = match ethtool_skel::ModSkelBuilder::default().open(&mut obj) {
        Err(e) => return (None, Err(e.to_string())),
        Ok(o)  => o,
    };
    let mut loaded = match open.load() {
        Err(e) => return (None, Err(e.to_string())),
        Ok(l)  => l,
    };
    if let Err(e) = loaded.attach() {
        return (None, Err(e.to_string()));
    }
    let skel: Box<ethtool_skel::ModSkel<'static>> =
        unsafe { std::mem::transmute(Box::new(loaded)) };
    (Some(skel), Ok(()))
}

fn load_rtnetlink() -> (Option<Box<rtnetlink_skel::ModSkel<'static>>>, Result<(), String>) {
    let mut obj = MaybeUninit::uninit();
    let open = match rtnetlink_skel::ModSkelBuilder::default().open(&mut obj) {
        Err(e) => return (None, Err(e.to_string())),
        Ok(o)  => o,
    };
    let mut loaded = match open.load() {
        Err(e) => return (None, Err(e.to_string())),
        Ok(l)  => l,
    };
    if let Err(e) = loaded.attach() {
        return (None, Err(e.to_string()));
    }
    let skel: Box<rtnetlink_skel::ModSkel<'static>> =
        unsafe { std::mem::transmute(Box::new(loaded)) };
    (Some(skel), Ok(()))
}
