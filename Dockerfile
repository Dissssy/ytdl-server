FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

# yt-dlp needs a JavaScript runtime for YouTube.
FROM denoland/deno:bin-2.9.8 AS deno

FROM python:3.12-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ffmpeg \
 && rm -rf /var/lib/apt/lists/*
# The server upgrades yt-dlp in this venv at startup and daily.
RUN python -m venv /opt/ytdlp \
 && /opt/ytdlp/bin/pip install --no-cache-dir --pre "yt-dlp[default,curl-cffi]"
COPY --from=deno /deno /usr/local/bin/deno
COPY --from=build /src/target/release/ytdl-server /usr/local/bin/ytdl-server
COPY web /app/web
ENV PATH=/opt/ytdlp/bin:$PATH \
    WEB_DIR=/app/web
CMD ["ytdl-server"]
