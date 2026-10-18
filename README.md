# DownloaDan

A fast, minimalist download manager for Windows: multi-connection downloads with resume, queues, a browser
extension that puts a download icon on videos, and safe, shareable themes.

Built with [Tauri 2](https://tauri.app) and Rust. It uses the WebView2 that already ships with Windows instead
of bundling a browser, so the installer is a few MB and it idles on a small fraction of the memory an Electron app needs.

## Build it yourself (one-time setup)
1. Install **Rust** from https://rustup.rs
2. Install the **Microsoft C++ Build Tools** (Visual Studio Installer -> "Desktop development with C++").
3. Install **Node.js LTS** (only used to run the Tauri command-line tool).
4. In this folder: `npm install`

Then:

| Command | What it does |
| --- | --- |
| `npm run dev` | Starts the app in development mode |
| `npm run build` | Builds the installer into `target\release\bundle\nsis\` |
| `npm test` | Runs the engine/manager/bridge tests |

The first build compiles everything and takes several minutes; after that it is quick.

## Project layout
- `core/` the engine, queue manager, browser bridge and theme rules, in Rust. No UI code, fully tested (`cargo test -p downloadan-core`).
- `src-tauri/` the app shell: windows, tray, notifications, updater, and the commands the pages can call.
- `ui/` the pages (plain HTML, CSS and JavaScript, no build step).
- `extension/` the browser extension (Chrome, Edge, Brave).

## Browser extension
Load the `extension` folder (`chrome://extensions` -> Developer mode -> Load unpacked). It opens a page that finds
DownloaDan by itself (it looks through ports 18642-18651) and DownloaDan asks you to **Allow** the connection. No token to copy.
Hover a video and click the icon in its top-right corner, or right-click any link or media and choose
**Download with DownloaDan**. DownloaDan always shows its Add box, so you confirm every download.

The extension has a fixed ID (set by the `key` in `extension/manifest.json`). If you publish it on a store, each store
gives it its own ID: add that ID to `ALLOWED_EXTENSION_IDS` in `core/src/api.rs`, and remove the `key` line from the
manifest in the copy you upload.

## Shortcuts
Ctrl+N add URL, Ctrl+A select all, Delete remove from list, double-click opens a finished file.

## Themes
Settings -> Appearance. Make, edit, duplicate, export (`.dantheme`) and import themes. A theme only contains 11 hex
colours, a corner radius, a density and a font from a fixed list. Imports are rejected if they contain anything else,
exceed 8 KB, or have unreadable contrast.

## Known limits
Adaptive/DRM streams (YouTube, Netflix, HLS/DASH playlists) are not supported; only direct files.

## Releasing an update
Updates are signed. The private key never goes in the repo; the public key lives in `src-tauri/tauri.conf.json`
(`plugins.updater.pubkey`), and installed copies refuse any update that does not verify against it.

**One-time setup**
1. Create the key pair: `npm run tauri signer generate -- -w %USERPROFILE%\.tauri\downloadan.key`
   (choose a password and keep the key file and password somewhere safe; if you lose them you cannot ship updates to installed copies).
2. Paste the printed **public** key into `plugins.updater.pubkey` in `src-tauri/tauri.conf.json`.
3. On GitHub: Settings -> Secrets and variables -> Actions -> add `TAURI_SIGNING_PRIVATE_KEY` (the contents of the `.key` file)
   and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

**Every release**
1. Raise `version` in `src-tauri/Cargo.toml` (it must always go up).
2. Commit, then push a tag: `git tag v2.0.1` and `git push origin v2.0.1`.
3. GitHub Actions builds the installer, signs it, and publishes the release with the update manifest (`latest.json`).
   Installed copies find it on start, or via Settings -> About.

Never edit or replace the files of a published release; publish a new version instead.

## Design note: native messaging later
`core/src/api.rs` holds all the browser-request logic and knows nothing about HTTP; `core/src/bridge.rs` is the HTTP transport.
`extension/bridge-client.js` is the only file in the extension that knows how to reach the app. A native-messaging
transport would be a second transport on each side, reusing the same operations and the same allowed-ID list.
