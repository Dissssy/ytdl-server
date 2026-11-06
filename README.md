# ytdl-server

A small invite-only web front end for yt-dlp. Paste a link, pick a preset or a size
limit, and download. NAS users' downloads go to the Plex folders instead.

## Running

`docker compose up -d --build`. Config lives in `/root/.config/ytdl-rs/config.json`
on the host and is re-read whenever it changes:

```json
{
  "host": "0.0.0.0",
  "port": 12221,
  "video_dir": "/mnt/Plex/YTDL/...",
  "audio_dir": "/mnt/Plex/YTDL/...",
  "tmp_dir": "/tmp/ytdlp-server",
  "max_file_size": 262144000,
  "auth_keys": [{ "user": "name", "key": "...", "nas": true }]
}
```

Users (run inside the container, `docker exec -it ytdl-server ytdl-server ...`):

| Command | Does |
| --- | --- |
| `-a NAME [--nas]` | Add a user and print their invite link |
| `-r NAME` | Remove a user |
| `--nas NAME on\|off` | Whether the user's downloads go to the NAS |
| `-l` | List users |

## Cookies

Each user can upload their own `cookies.txt` in Settings (or the extension's
options). It's stored as `cookies/<user>.txt` next to config.json, mode 600, and
used only for that user's downloads. Users without one fall back to the shared
file at `COOKIES_PATH`. Sign-in errors tell the user to upload theirs.

## API

Legacy, kept for old scripts: `GET /download` (NAS users only, returns a job id),
`GET /check/{id}`, `GET /download_now`, `GET /get_info`.

JSON, with `Authorization: Bearer KEY` or `?key=KEY`: `GET /api/me`,
`POST /api/probe {url}`, `POST /api/jobs {url, mode: compat|best|audio, max_size?, format?, target: browser|nas|auto}`,
`GET /api/jobs`, `GET /api/jobs/{id}`, `DELETE /api/jobs/{id}`,
`GET /api/jobs/{id}/file`, `GET /api/events` (server-sent events),
`POST /api/cookies` (cookies.txt as the body) and `DELETE /api/cookies`.

## Firefox extension

`extension/` is a small Firefox add-on: right-click "Download video/audio" on any
page, link or video, a toolbar popup with progress, and the
`dnrNAMESPACE.download_video/download_audio` functions for Gesturify. It signs
itself in from the site when you visit it signed in.

To install it permanently it must be signed (free, not published):

1. Zip the folder's contents: `cd extension && zip -r ../media-downloader.zip .`
2. Upload the zip at https://addons.mozilla.org/developers/addon/submit/distribution
   and choose "On your own".
3. Download the signed `.xpi`, save it as `web/media-downloader.xpi`, and rebuild.
   The page then shows Firefox users an install link.

For testing, `about:debugging` > This Firefox > Load Temporary Add-on > `extension/manifest.json`.
