FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release \
 && cp target/release/ytdl-server /ytdl-server

FROM python:3.12-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ffmpeg \
 && rm -rf /var/lib/apt/lists/*
# The server calls /root/.pyenv/shims/python (to pip-upgrade yt-dlp) and
# /root/.pyenv/shims/yt-dlp by absolute path, and yt-dlp by name via ytd-rs.
# A venv on PATH plus shim links satisfies all three without code changes.
RUN python -m venv /opt/ytdlp \
 && /opt/ytdlp/bin/pip install --no-cache-dir --pre "yt-dlp[default,curl-cffi]" \
 && mkdir -p /root/.pyenv/shims \
 && ln -s /opt/ytdlp/bin/python /root/.pyenv/shims/python \
 && ln -s /opt/ytdlp/bin/yt-dlp /root/.pyenv/shims/yt-dlp
ENV PATH=/opt/ytdlp/bin:$PATH \
    COOKIES_PATH=/root/cookies.txt
COPY --from=build /ytdl-server /usr/local/bin/ytdl-server
CMD ["ytdl-server"]
