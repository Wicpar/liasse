# Feasibility prototype for the liasse-pg-ext design: build a minimal pgrx
# extension against PG 17 and ship it in the official postgres:17 image —
# the same two-stage shape the real image uses.

# ---- build stage: rust + PGDG server headers + cargo-pgrx ----
FROM rust:1-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends \
        curl ca-certificates gnupg clang libclang-dev pkg-config \
    && install -d /usr/share/postgresql-common/pgdg \
    && curl -fsSL https://www.postgresql.org/media/keys/ACCC4CF8.asc \
        -o /usr/share/postgresql-common/pgdg/apt.postgresql.org.asc \
    && echo "deb [signed-by=/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc] https://apt.postgresql.org/pub/repos/apt bookworm-pgdg main" \
        > /etc/apt/sources.list.d/pgdg.list \
    && apt-get update && apt-get install -y --no-install-recommends \
        postgresql-server-dev-17 postgresql-17 \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-pgrx --locked
RUN cargo pgrx init --pg17 /usr/lib/postgresql/17/bin/pg_config
WORKDIR /
RUN cargo pgrx new liasse_demo
WORKDIR /liasse_demo
RUN cargo add serde --features derive && cargo add serde_json
COPY lib.rs src/lib.rs
RUN cargo pgrx package --pg-config /usr/lib/postgresql/17/bin/pg_config

# ---- runtime stage: stock postgres:17 + the packaged extension ----
FROM postgres:17-bookworm
COPY --from=build /liasse_demo/target/release/liasse_demo-pg17/usr/lib/postgresql/17/lib/ \
    /usr/lib/postgresql/17/lib/
COPY --from=build /liasse_demo/target/release/liasse_demo-pg17/usr/share/postgresql/17/extension/ \
    /usr/share/postgresql/17/extension/
COPY initdb.sql /docker-entrypoint-initdb.d/10-liasse.sql
