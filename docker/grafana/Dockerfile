FROM grafana/grafana:11.5.2
USER root
RUN mkdir -p /grafana-plugins && \
    grafana cli --pluginsDir /grafana-plugins plugins install grafana-clickhouse-datasource && \
    chown -R grafana:root /grafana-plugins
