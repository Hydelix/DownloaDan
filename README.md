# DownloaDan

Lightweight download manager for Windows: multi-connection downloads with resume, queues,
a browser extension that puts a download icon on videos, and shareable (safe) themes.

## Run it
1. Install Node.js LTS from https://nodejs.org (Windows 10/11, x64).
2. In this folder: `npm install` then `npm start`.

## Build an installer
`npm run dist` -> `dist\DownloaDan Setup 1.0.0.exe` (unsigned, so SmartScreen may warn on first run).

## Browser extension (Chrome, Edge, Brave)
1. Open `chrome://extensions` (Edge: `edge://extensions`), enable **Developer mode**.
2. **Load unpacked** and choose the `extension` folder.
3. In DownloaDan: **Settings -> Browser -> Copy** (pairing token).
4. Click the DownloaDan extension icon, paste the token, **Save**, then **Test connection**.

Hover a video and click the icon in its top-right corner, or right-click any link or media
and choose **Download with DownloaDan**. DownloaDan always shows its Add dialog so you confirm every download.

## Shortcuts
Ctrl+N add URL, Ctrl+A select all, Delete remove from list, double-click opens a finished file.

## Themes
Settings -> Appearance. Make, edit, duplicate, export (`.dantheme`) and import themes.
A theme only contains 11 hex colours, a corner radius, a density and a font from a fixed list.
Imports are rejected if they contain anything else, exceed 8 KB, or have unreadable contrast.

## Known limits
Adaptive/DRM streams (YouTube, Netflix, HLS/DASH playlists) are not supported; only direct files.

## Releasing an update (GitHub)
Releases are published on this same (public) repo: `package.json` -> `build.publish` points at `Hydelix/DownloaDan`.
The repo must stay public, otherwise installed copies can't check for updates without a token.

Every release:
1. Raise `"version"` in `package.json` (1.0.0 -> 1.0.1). It must always go up.
2. `npm run dist`
3. On GitHub: Releases -> Create a new release -> new tag `v1.0.1` (the letter v + the exact version).
4. Attach exactly these three files from `dist`: `DownloaDan-Setup-1.0.1.exe`, `DownloaDan-Setup-1.0.1.exe.blockmap`, `latest.yml`.
5. Publish release (not draft, not pre-release). Installed copies find it on start, or via Settings -> About.

Never edit or replace the files of a release after publishing; rebuild with a new version instead.

## Connecting the browser extension
Load the `extension` folder (chrome://extensions -> Developer mode -> Load unpacked). It opens a page that finds DownloaDan
by itself (it looks through ports 18642-18651) and DownloaDan asks you to Allow the connection. No token to copy.
The extension has a fixed ID (set by the `key` in `extension/manifest.json`), so it stays the same wherever the folder lives.

If you publish the extension on a store (Chrome Web Store / Edge Add-ons), each store gives it its own ID:
add that ID to `src/main/extension-ids.js`, and remove the `key` line from the manifest in the copy you upload.

## Design note: adding native messaging later
`src/main/api.js` holds all the logic and knows nothing about HTTP; `src/main/bridge.js` is the HTTP transport.
`extension/bridge-client.js` is the only file in the extension that knows how to reach the app.
A native-messaging transport would be a second transport on each side, reusing the same operations and the same allowed-ID list.

