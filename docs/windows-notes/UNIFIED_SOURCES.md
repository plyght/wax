# Unified Windows-oriented package sources in wax

wax can treat **Homebrew** (cached index), **Scoop** (Main bucket JSON), **winget-pkgs** (GitHub YAML portable zips), and **Chocolatey** (community `.nupkg`) as install/search targets. Downloads use wax’s **multipart HTTP** where applicable; no Scoop PowerShell and no `winget.exe` are required for the supported subsets.

## Bang prefixes

| Prefix | Meaning |
|--------|---------|
| `scoop/` | Force Scoop Main manifest (`scoop/ripgrep`). |
| `choco/` or `chocolatey/` | Force Chocolatey gallery id (`choco/git`). |
| `winget/` | Force winget **PackageIdentifier** (`winget/JesseDuffield.lazygit`). |
| `brew/` or `homebrew/` | Force Homebrew-style resolution (`brew/openssl`). |
| *(none)* | **Auto**: on Windows, probe all sources in parallel and pick the **fastest** tier that matches (brew → scoop → winget → chocolatey). |

Tap-style names (`user/repo/formula`) and version pins (`pkg@version`) skip auto-routing and use the normal Homebrew path.

## “Fastest” when names collide

`Ecosystem::speed_rank` (lower = preferred): **brew (0) < scoop (1) < winget (2) < chocolatey (3)**.

- **`wax search`**: Homebrew formulae/casks are listed first; remote hits that duplicate an existing formula or cask **name** are hidden so the faster catalogue wins.
- **Remote-only dedupe**: If the same id appears in Scoop, winget, and Chocolatey, the **fastest** source is kept.

## What is actually installed

| Source | wax behaviour |
|--------|----------------|
| **brew** | Existing bottle/source/cask flows. |
| **scoop** | JSON manifest → zip/tar.gz → `~/.local/wax/bin` (Windows) + Wax manifest state. No `pre_install` / `installer` scripts. |
| **winget** | Latest version under `manifests/<letter>/…` on **microsoft/winget-pkgs**; portable zip installs and native MSI/MSIX/EXE installs when the manifest has checksum, silent install data, and managed uninstall metadata. |
| **chocolatey** | Latest `.nupkg` → extract → copy `tools/**/*.exe` (filters obvious uninstall/choco helpers) + Wax manifest state. Script-only packages fail with a clear error. |

Chocolatey script-driven native installers remain out of scope until Wax has a constrained Chocolatey helper parser. Wax does not run arbitrary package PowerShell.

## Search

- **Unified** (no prefix): Homebrew + Scoop Main index (GitHub tree, cached 24h) + Chocolatey HTML search + winget-pkgs manifest index (git metadata cache, cached 24h). No `GITHUB_TOKEN` is required.
- **Prefixed** (`scoop/foo`, …): Only that catalogue.

## Lifecycle

- `wax list` includes Wax-managed Scoop, winget, and Chocolatey portable installs as `scoop/id`, `winget/id`, or `choco/id`.
- `wax uninstall scoop/id`, `wax uninstall winget/id`, and `wax uninstall choco/id` remove the recorded `~/.local/wax/bin` links, staged files, staging directory, and Wax manifest.
- For winget native installers, `wax uninstall winget/id` runs the recorded native uninstall command before removing Wax state.
- `wax uninstall --all` includes these Windows portable manifests.
- `wax upgrade scoop/id`, `wax upgrade winget/id`, and `wax upgrade choco/id` reinstall the recorded package through Wax’s own source backend.
- Install refuses to overwrite a `~/.local/wax/bin` executable already owned by another Windows manifest.

## Related files

- `src/package_spec.rs` — bang parsing and speed ordering.
- `src/ecosystem_install.rs` — auto pick + forced install routing.
- `src/remote_search.rs` — merged search and dedupe.
- `src/windows_state.rs` — Wax-owned Windows portable package manifests.
- `src/scoop.rs`, `src/winget_install.rs`, `src/chocolatey.rs` — download + extract + shim to `~/.local/wax/bin`.

See also [DESK_RESEARCH.md](DESK_RESEARCH.md) and [WINDOWS_PACKAGE_MANAGER_INVESTIGATION.md](../WINDOWS_PACKAGE_MANAGER_INVESTIGATION.md).
