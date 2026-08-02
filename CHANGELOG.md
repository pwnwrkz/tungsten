# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Global `--verbose` flag that enables debug-level log output for troubleshooting
- Debug logging across upload, sync, init, watch, config parsing, and API key resolution paths

### Changed

- Replaced loose per-image parameters in individual sync with a `ProcessImageCtx` context struct
- Bumped `clap` to 4.6.5 and `resvg`/`usvg` to 0.48
- Capitalized app name in help output

### Removed

- Removed LucideRoblox origin note and Windows-only disclaimer from README

## [v3.0.0]

### Added

- Configurable upload concurrency via `max_concurrent_uploads` in `tungsten.toml` (default: 10)
- Studio sync cleanup logic to remove stale files from `.tungsten-debug/` folder
- File tracking mechanism for Studio sync to track expected files during sync operations
- `StudioConfig` section with `studio_path` and `auto_route_version` for advanced Studio path handling
- Automatic version routing via `https://setup.roblox.com/versionQTStudio` when `auto_route_version` is enabled

### Changed

- Changed Studio sync behavior from wiping previous contents to incremental sync that preserves assets between Studio version updates
- Updated SVG scaling to use per-file scale based on viewBox rather than global input scale
- Changed log output format from symbols (∙, ✓, ⚠, ✗) to bracketed labels ([INFO], [SUCCESS], [WARNING], [ERROR]) for better readability
- Changed progress bar format: removed leading spaces, added zero-padded counters aligned to total width, and updated completion line to use [SUCCESS] label
- Changed API key loading to use standard `.env` files with `TUNGSTEN_API_KEY` instead of `tungsten_api_key.env` files
- Improved variable naming in `src/core/assets/img/alpha_bleed.rs` for BFS algorithm readability
- Added bleed configuration option to inputs to control alpha bleeding (defaults to true for backward compatibility)
- Implemented automatic spritesheet packing similar to Adobe Animate:
  - Sorts sprites by largest height first, then largest width first
  - Uses rect packing algorithm with upright-only sprite placement
  - Dynamically sizes atlases (calculates needed size, increases only when necessary)
  - Enforces maximum atlas size of 1024x1024
  - Automatically generates additional atlases when needed
  - Trims final atlases to actual used space (removes empty padding)
- Modified spritesheet packing to always use maximum atlas size (1024x1024) to minimize the number of sheets while trimming unused space
- Optimized alpha bleeding algorithm (`alpha_bleed.rs`) — ~10-50x faster for spritesheets via bit-packed `Vec<u32>`, ring buffer, and 4-neighbor fast path
- Parallel DPI variant pre-processing and upload (2x, 3x via Rayon)
- Parallel spritesheet bleed/encode/compress for multiple atlases
- Lockfile hashing optimization using `hex::encode()` for SHA-256 digest formatting

### Fixed

- Fixed studio sync incorrectly changing file extensions for audio and model assets (e.g., .mp3 to .audio, .rbxm to .model) when syncing to Studio target

### Removed

- Removed adding `tungsten_api_key.env` into the project's `.gitignore` as it's no longer being used
- Removed DPI variant packing support; high DPI variants are skipped for packing (waitlisted for manual upload) but still generate DPI group code entries

## [v3.0.0-rc.2]

### Added

- Added studio sync cleanup logic to remove stale files from `.tungsten-debug/` folder
- Added file tracking mechanism for Studio sync to track expected files during sync operations
- Added `StudioConfig` section with `studio_path` and `auto_route_version` for advanced Studio path handling
- Added automatic version routing via `https://setup.roblox.com/versionQTStudio` when `auto_route_version` is enabled

### Changed

- Changed Studio sync behavior from wiping previous contents to incremental sync that preserves assets between Studio version updates
- Updated SVG scaling to use per-file scale based on viewBox rather than global input scale
- Changed log output format from symbols (∙, ✓, ⚠, ✗) to bracketed labels ([INFO], [SUCCESS], [WARNING], [ERROR]) for better readability
- Changed progress bar format: removed leading spaces, added zero-padded counters aligned to total width, and updated completion line to use [SUCCESS] label
- Changed API key loading to use standard `.env` files with `TUNGSTEN_API_KEY` instead of `tungsten_api_key.env` files

### Fixed

- Fixed studio sync incorrectly changing file extensions for audio and model assets (e.g., .mp3 to .audio, .rbxm to .model) when syncing to Studio target

### Removed

- Removed adding `tungsten_api_key.env` into the project's `.gitignore` as it's no longer being used.

## [v3.0.0-rc.1]

### Added

- CHANGELOG.md file to track changes between versions
- **Breaking:** Added required `type` field to inputs, allowing specifying asset type (e.g., decal, image) independent of file kind.

### Changed

- Improved variable naming in `src/core/assets/img/alpha_bleed.rs` for better readability:
  - Replaced single-letter variables (`w`, `h`, `len`, `i`, `x`, `y`) with descriptive names (`width`, `height`, `pixel_count`, `index`, `x`, `y`)
  - Improved clarity in BFS algorithm with more descriptive variable names (`red_sum`, `green_sum`, `blue_sum`, `sample_count`)
  - Renamed queue variables for clarity (`current` -> `current_wave`, `next` -> `next_wave`)
- Updated ignore reason in `src/core/assets/img/convert.rs` test:
  - Added descriptive reason to ignored test: `#[ignore = "TGA support not fully tested in CI environment"]`
- Improved documentation accuracy:
  - Fixed creator configuration example in docs/getting-started/first-sync.mdx to show correct `[creator]` format
  - Corrected debug folder naming in docs/reference/cli.md from .tungsten_debug to .tungsten-debug to match implementation
  - Enhanced meta file documentation in docs/reference/meta-files.mdx to explain the naming convention priority:
    - For files: tries `name.format.tmeta` first (e.g., `logo.png.tmeta`), then falls back to `name.tmeta` (e.g., `logo.tmeta`)
    - For directories: uses `name.tmeta` (e.g., `icons.tmeta`)
  - Improved meta file handling in src/core/assets/asset.rs to implement the dual naming convention:
    - Files check for `name.format.tmeta` first, then `name.tmeta`
    - Directories use `name.tmeta`
    - Added comprehensive tests for meta file naming behavior
- Added bleed configuration option to inputs to control alpha bleeding processing (defaults to true for backward compatibility)
- Implemented automatic spritesheet packing similar to Adobe Animate:
  - Sorts sprites by largest height first, then largest width first
  - Uses rect packing algorithm with upright-only sprite placement
  - Dynamically sizes atlases (calculates needed size, increases only when necessary)
  - Enforces maximum atlas size of 1024x1024
  - Automatically generates additional atlases when needed
  - Trims final atlases to actual used space (removes empty padding)
- Modified spritesheet packing to always use maximum atlas size (1024x1024) to minimize the number of sheets while trimming unused space.

### Fixed

- No fixes in this release

### Removed

- Removed DPI variant packing support; high DPI variants are skipped for packing (waitlisted for manual upload) but still generate DPI group code entries
