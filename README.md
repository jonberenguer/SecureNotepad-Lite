# SecureNote

A cross-platform native desktop notepad with AES-256-GCM encryption. No web browser, no Node.js, no server — just a single compiled binary.

---

## Features

| | |
|---|---|
| 🔐 | **AES-256-GCM** encryption with **PBKDF2-SHA256** key derivation (310,000 iterations) |
| 📑 | Up to **5 named encrypted tabs** |
| 💾 | **Auto-save** with configurable delay; always saves on window close |
| 🔍 | **Find & Replace** with match counter |
| 🎨 | **Dark / Light** theme toggle |
| 🖱️ | **Ln / Col** cursor position in status bar |
| 📁 | **Data directory path** shown in status bar |
| ⚙️ | **Preferences** panel — font size, auto-save delay, theme (persisted to `prefs.json`) |
| 🪟 | **Window position and size** restored on next launch |
| 🔒 | **Single instance** enforcement via PID lock file |
| 🖼️ | **Custom app icon** — drop `icon.png` in the data directory, no recompile needed |
| 🛡️ | Constant-time password comparison, change password with re-encryption, erase-all |
| 🔗 | **Wire-format compatible** — `notes.enc` files are interchangeable with the web version |

---

## Building

Install Rust if you haven't already:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then build:

```bash
cargo build --release
```

The binary will be at:

- **Linux / macOS:** `target/release/secure-note`
- **Windows:** `target/release/secure-note.exe`

No additional runtime dependencies are required. egui uses the platform's native GPU backend (OpenGL on Linux/Windows, Metal on macOS).

---

## Running

```bash
# Default data directory: ./secure-notes/
./secure-note

# Custom data directory
./secure-note --data /path/to/my/notes
```

The browser **does not open** — this is a native desktop window. On first launch you will be prompted to set a master password. **This password cannot be recovered.**

---

## Data Directory Layout

```
secure-notes/
├── notes.enc       ← AES-256-GCM encrypted tab contents (base64)
├── config.json     ← PBKDF2 password hash  (salt:hex_hash)
├── prefs.json      ← UI preferences (font size, theme, window geometry, auto-save)
├── app.lock        ← PID lock file (single-instance enforcement, auto-removed on exit)
└── icon.png        ← Optional custom app icon (PNG, any size)
```

---

## Keyboard Shortcuts

| Shortcut | Action |
|---|---|
| `Ctrl+S` | Save all tabs |
| `Ctrl+L` | Save and lock (return to password screen) |
| `Ctrl+F` | Open Find bar |
| `Ctrl+H` | Open Find & Replace bar |
| `Ctrl+T` | New tab |
| `Ctrl+1` … `Ctrl+5` | Switch to tab 1–5 |
| `Ctrl+,` | Toggle Preferences panel |
| `Escape` | Close Find bar / close Preferences panel |

---

## Custom Icon

Place any PNG file named `icon.png` in the data directory (e.g. `./secure-notes/icon.png`) and restart the app. The image will be used as the window and taskbar icon. No recompile is needed.

---

## Single Instance

On launch, SecureNote writes its PID to `app.lock` in the data directory. If another instance is already running, the new launch prints an error and exits immediately. The lock file is removed automatically on clean exit.

If the app crashes and leaves a stale lock file, simply delete `app.lock` manually before relaunching.

---

## Encryption Details

All notes are stored in a single encrypted file (`notes.enc`). The format is:

```
base64( salt[32 bytes] || iv[12 bytes] || auth_tag[16 bytes] || ciphertext )
```

- **Key derivation:** PBKDF2-SHA256, 310,000 iterations
- **Cipher:** AES-256-GCM (authenticated encryption — any tampering is detected)
- **Password storage:** PBKDF2-SHA256 hash stored in `config.json` as `hex_salt:hex_hash`
- **Password verification:** constant-time comparison (`subtle` crate) to prevent timing attacks
- **Session:** in-memory only, no token written to disk

This format is identical to the original Node.js web version, so `notes.enc` files are fully interchangeable between the two.

---

## Platform Support

| Platform | GPU Backend |
|---|---|
| **Linux** | OpenGL (X11 and Wayland) |
| **macOS** | Metal |
| **Windows** | DirectX 12 / OpenGL fallback |

The `#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]` attribute suppresses the console window on Windows release builds.

---

## Dependencies

| Crate | Purpose |
|---|---|
| `eframe` / `egui` | Native GUI framework |
| `aes-gcm` | AES-256-GCM authenticated encryption |
| `pbkdf2` | PBKDF2 key derivation |
| `sha2` / `hmac` | SHA-256 and HMAC for PBKDF2 |
| `rand` | Cryptographically secure random bytes |
| `subtle` | Constant-time byte comparison |
| `hex` | Hex encoding/decoding |
| `base64` | Base64 encoding/decoding |
| `serde` / `serde_json` | JSON serialization for config and prefs |
| `clap` | CLI argument parsing (`--data`, `--help`) |
