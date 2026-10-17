# tunebridge

A small Matrix bot that keeps a music-sharing chat working when the people in
it use different streaming services. Paste a Spotify link and it replies with
the same track or album on YouTube Music; paste a YouTube Music link and it
replies with Spotify. It works with any Matrix client because the reply is an
ordinary quoted message.

```
you:        check this out https://open.spotify.com/track/0VjIjW4GlUZAMYd2vXMi3b 🔥
tunebridge: ↳ 🎵 Blinding Lights – The Weeknd ✨🎤
            🔴 YouTube Music: https://music.youtube.com/watch?v=4NRXx6U8ABQ
```

Each target link is prefixed with a service badge: 🟢 for Spotify and 🔴 for
YouTube Music, and in clients that render HTML the service name is also shown
in the brand colour. When a genre is known (Spotify artist genres, YouTube
topic categories) a small genre emoji set follows the title, e.g. 🤘🎸 for
rock or 🎷🌙 for jazz; there are several sets per genre and the pick is
stable per song. Turn it off with `genre_emojis = false`. Links are found
anywhere inside a message, surrounded by other text or not.

Supported inputs: `open.spotify.com/track|album/…`, `spotify:track:…` URIs,
`spotify.link` short links, `music.youtube.com/watch?v=…`,
`music.youtube.com/playlist?list=OLAK5uy_…` (albums) and
`music.youtube.com/browse/MPREb_…` album pages. Plain `youtube.com` /
`youtu.be` links are ignored unless `translate_plain_youtube = true`.
Playlists are out of scope.

Encrypted rooms are supported; the bot keeps its device keys in `data_dir`.

## How matching works

There is no public cross-service id, so the bot resolves the shared link on its
home service (title, artists, duration, ISRC on Spotify), searches the other
service, and scores each hit on title similarity, artist overlap and duration
(±3 s is a full match, >15 s is rejected). Live / remix / acoustic variants are
penalised against studio versions. YouTube "Artist - Topic" uploads and
`OLAK5uy_` album playlists get a small bonus because they are the canonical
YouTube Music entries. Results are cached for 24 h.

## Setup

### 1. Credentials

- **Matrix user**: register a normal (non-admin) account for the bot on your
  homeserver, e.g. on a Synapse container:
  `docker exec synapse register_new_matrix_user -c /data/homeserver.yaml -u tunebridge -p '<password>' --no-admin http://localhost:8008`
- **Spotify**: <https://developer.spotify.com/dashboard> → *Create app*. Any
  redirect URI is fine; the bot uses the client-credentials flow and needs no
  user scopes. Copy the client id and secret.
- **YouTube**: <https://console.cloud.google.com> → create/select a project →
  *APIs & Services* → enable **YouTube Data API v3** → *Credentials* → *API
  key*. The free quota (10 000 units/day) allows roughly 100 translations a
  day; each one costs one search (100 units) plus a couple of list calls.

### 2. Configure

```sh
cp config.example.toml config.toml   # git-ignored
$EDITOR config.toml
```

Secrets may instead be passed as `TUNEBRIDGE_MATRIX_PASSWORD`,
`TUNEBRIDGE_SPOTIFY_CLIENT_ID`, `TUNEBRIDGE_SPOTIFY_CLIENT_SECRET` and
`TUNEBRIDGE_YOUTUBE_API_KEY`. The config path comes from `TUNEBRIDGE_CONFIG`,
the first CLI argument, or `./config.toml`.

### 3. Run

Locally (Nix dev shell provides the toolchain):

```sh
nix develop
cargo test
cargo run -- config.toml
```

Then invite the bot user to your room from your own account. Invites from
users listed in `auto_join_invites_from` are accepted automatically.

The first run logs in with the password and writes `session.json` plus the
sqlite stores into `data_dir`; later runs restore that session, so the bot
keeps the same device id and encryption keys. Delete `data_dir` only if you
also want a fresh device.

Element X will flag the bot as an unverified device the first time. That is
cosmetic; messages are still delivered. You can verify it later from your own
session if you want the green shield.

### 4. Deploy as a container

A prebuilt image is published to `ghcr.io/optimosupreme/tunebridge` by the
`image` GitHub Actions workflow on every push to `main`, on version tags, and
on a monthly schedule so `latest` picks up base-image fixes. Use it with
`image: ghcr.io/optimosupreme/tunebridge:latest` if you would rather pull than
build.


The image is a plain multi-stage build (`Dockerfile`). `deploy.sh` builds it
with podman on your workstation, streams it to the server with
`podman save | ssh host docker load`, and runs
`docker compose up -d --no-deps tunebridge`, so nothing is ever compiled on
the server and no other service is restarted. The compose service it expects:

```yaml
  tunebridge:
    image: tunebridge:latest
    pull_policy: never
    container_name: tunebridge
    restart: unless-stopped
    volumes:
      - ./tunebridge/data:/data
      - ./tunebridge/config.toml:/config.toml:ro
    depends_on:
      - synapse
```

Inside the same compose network the homeserver URL can simply be
`http://synapse:8008`. The container runs as uid 10001, so
`chown -R 10001 tunebridge/data` and make `config.toml` readable by that uid
(e.g. `chmod 640 config.toml && chown 10001:$USER config.toml`) once on the host.

## Development

```
src/links.rs        URL / URI recognition (unit-tested for every supported shape)
src/matching.rs     normalisation + scoring (table-driven tests; tune weights here)
src/providers/      Spotify Web API and YouTube Data API clients
src/translate.rs    orchestration, caching, reply formatting
src/matrix.rs       Matrix event handling and replies
```

`cargo clippy -- -D warnings` and `cargo fmt` are expected to be clean.
Adding another service means implementing `resolve` and `search` in a new
provider module and adding a `Service` variant.

## License

MIT
