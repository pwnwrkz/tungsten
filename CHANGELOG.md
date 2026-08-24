# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [3.1.1] - 2026-08-24

### Added

- Unified sync dispatcher (`sync::dispatch`) shared by the individual, raw, and packed processors: dry-run, lockfile cache checks, Studio/Debug copies, and cloud uploads now run through identical code paths for every mode
- Shared DPI group processing (`sync::dpi::process_dpi_groups`) used by both individual and packed sync, emitting exactly one `dpi_group` codegen entry per base name
- Lock-free rate limiting in the upload client: atomic rate-limit window, jittered waits, and retry of transient network errors and 5xx responses with exponential backoff capped at 30s
- Early cancellation of in-flight uploads when a fatal error occurs, via a shared `tokio::sync::Notify`
- Streaming multipart uploads with `bytes::Bytes` payloads so retries clone in O(1) instead of copying the file
- In-memory image compression (no temp files) that keeps the original bytes when compression would not save space
- Direct SVG rasterization to straight-alpha RGBA via resvg's `take_demultiplied`, skipping the previous PNG encode/decode roundtrip
- Lazy decoding of on-disk raster images inside the blocking pool to bound memory by processing concurrency
- Memoized `.tmeta` SVG scale lookups (`SvgScaleCache`) so a directory's meta file is read once per sync
- Per-thread alpha-bleed buffer reuse to avoid re-allocating per image
- Streaming codegen output with a unified flat/nested entry writer
- Cached `create_dir_all` calls in Studio and debug sync to skip redundant syscalls
- Unit tests for DPI grouping/processing, upload backoff/jitter/rate-limit, compression fallback, alpha-bleed buffer reuse, and codegen; `serial_test` added for env and lockfile tests
- Local upload test suite with bundled sample assets under `tests/assets/`

### Changed

- `sync` and `watch` target arguments are now a clap value enum instead of a free-form string
- Packed DPI groups are uploaded/copied per variant instead of being waitlisted with placeholder codegen entries
- Upload client sets the API key once as a default header instead of on every request
- Studio version routing (`auto_route_version`) now uses async requests — the `reqwest` blocking feature is gone
- `resolve_api_key` accepts `&str` instead of `Option<String>`
- `Config`, `CreatorConfig`, `CodegenConfig`, and `StudioConfig` now derive `Clone`, and codegen config exposes resolved helper methods
- `maybe_compress_png` returns the original bytes when compression fails or yields no saving
- Alpha-bleed wavefront queues are bounded by image perimeter and reused across images via thread-locals
- Bumped `clap` to 4.6.6, `resvg`/`usvg` to 0.48.1, and `quinn-proto` to 0.11.16; added the `bytes` dependency and `http`/`serial_test` dev-dependencies; removed `futures-util`
- CI workflows migrated to Blacksmith runners and the `setup-rust-toolchain` action
- Docs rewritten across getting-started, guides, and reference pages; bumped `astro`, `starlight`, and `sharp`

### Fixed

- Packed images were dropped from codegen output when packed; their codegen entries are now preserved
- DPI groups could emit duplicate/nondeterministic codegen entries on cache hits — one entry per base name now
- `maybe_compress` could return empty bytes when format normalization failed, uploading a corrupt asset
- SVG rasterization produced premultiplied alpha; output is now straight alpha

### Removed

- Ignored TGA conversion test (TGA support not exercised in CI)
- Dead code: `Lockfile::force_save`, `Lockfile::get_uri`, `StudioSync::asset_uri`/`identifier`, and `CodegenEntry::sprite_id`

## [v3.1.0] - 2026-08-03

### Added

- Web asset support: `WebAsset` type with automatic animation detection for `rbxm`/`rbxmx`/`fbx` files
- Optional `asset_type` and web asset mapping in input configuration, with per-input `asset_type` override resolution propagated to all sync processors
- Web asset support in individual, packed, and raw sync processors, all sharing a common compress helper
- `seed_web_assets` codegen helper and simplified `write_codegen` output
- Rewritten `init` command with interactive prompts for creator, folder selection, and codegen output
- Interactive folder selector in `init` with type cycling and multi-select fallback
- Global `--verbose` flag that enables debug-level log output for troubleshooting
- Debug logging across upload, sync, init, watch, config parsing, and API key resolution paths

### Changed

- Log formatting updated to colored backgrounds and section headers
- Capitalized section headers in the test and watch commands
- Replaced loose per-image parameters in individual sync with a `ProcessImageCtx` context struct
- Moved `maybe_compress_png` to the public API and removed the unused parallel variant
- Bumped `clap` to 4.6.5 and `resvg`/`usvg` to 0.48
- Capitalized the app name in help output

### Fixed

- Punctuation in CLI error messages (removed stray trailing period)

### Removed

- LucideRoblox origin note and Windows-only disclaimer from README

## [v3.0.0] - 2026-07-14

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

## [v3.0.0-rc.2] - 2026-07-13

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

## [v3.0.0-rc.1] - 2026-06-27

### Added

- CHANGELOG.md file to track changes between versions
- **Breaking:** Added required `type` field to inputs, allowing specifying asset type (e.g., decal, image) independent of file kind.
- Added optional `type` field to Tungsten metadata files, allowing specifying asset types for specific assets.

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

## [v2.1.1] - 2026-04-27

### Changed

- Removed unused compression parameters from the compression API and input configuration
- Simplified compression config defaults and CI checks

## [v2.1.0] - 2026-04-27

### Added

- Image compression powered by libcaesium, with per-input quality settings (`jpeg_quality`, `png_quality`, `webp_quality`, `optimize_gif`, `keep_metadata`)
- Raw sync processor for non-image assets (audio, models, animations)
- Split the sync command into focused modules (`individual`, `packed`, `raw`, `paths`, `codegen_write`) and reorganized `core` into `assets` and `postsync`

### Changed

- Replaced the conversion pipeline with compression: unsupported formats are normalized for compression (e.g., BMP/TGA to PNG) instead of unconditionally converted
- Rewrote alpha bleeding with a new algorithm and tests
- Expanded configuration with per-input compression options

## [v2.0.1] - 2026-04-19

### Fixed

- `init` now writes `packable = true` for image directories so icons and sprites are packed by default

### Changed

- Updated dependencies in `Cargo.lock`
- Release workflow now publishes to crates.io

## [v2.0.0] - 2026-04-19

### Added

- Watch command that re-syncs automatically on file changes with debouncing, similar to `rojo serve`
- Studio sync target: copies assets into the Roblox Studio content folder with stable `rbxasset://` URIs for live preview
- Debug sync target: copies assets into a local `.tungsten_debug/` folder to inspect sync output without touching Roblox
- Alpha bleeding to fill transparent spritesheet edges
- Rewritten `init` command with interactive prompts and automatic detection of common asset folders
- DPI variant detection: `@2x`/`@3x` suffixes are grouped per base name and emit `function(dpiScale)` codegen entries
- Codegen expansion: flat and nested styles, optional extension stripping, and TypeScript `.d.ts` declaration output
- `Target` enum for `cloud`, `studio`, and `debug` sync destinations

### Changed

- Reworked codegen to support sprite entries referencing packed spritesheets
- Lockfile now tracks Studio URIs alongside cloud asset IDs

## [v1.0.0] - 2026-04-06

### Added

- First stable release
- Conversion rules (`ConvertRules`): declarative per-input format conversion with `"from -> to"` syntax, supporting extension-wide and file-specific rules
- SVG rasterization to PNG via `resvg`, with a per-input `svg_scale`
- Typed asset kinds (`ImageFormat`, `AudioFormat`, `ModelFormat`, `Animation`) and a unified `UploadParams` struct
- `.tmeta` metadata files to override asset names and descriptions
- Raw asset processing for audio, models, and animations
- Codegen entries for both packed sprite regions and plain assets

## [v0.1.3] - 2026-03-31

### Added

- API key resolution from a project `tungsten_api_key.env` file (`API_KEY=...`) and the `TUNGSTEN_GLOBAL_APIKEY` environment variable, in addition to the `--api-key` flag

### Changed

- `init` now adds `tungsten_api_key.env` to `.gitignore`
- Improved `test` and codegen output

## [v0.1.2] - 2026-03-29

### Changed

- Reworked the upload pipeline around a shared connection-pooled HTTP client so uploads reuse TCP connections
- Rate limiting now prefers the server's `x-ratelimit-reset` header and falls back to exponential backoff
- Improved error messages for missing API keys and upload timeouts
- Rewritten sync, init, codegen, and lockfile paths

## [v0.1.1] - 2026-03-25

### Fixed

- Spritesheet packing overflow issue

### Changed

- Tungsten-specific `.gitignore` entries and release workflow updates

## [v0.1.0] - 2026-03-25

### Added

- Initial release
- Sync images to the Roblox Open Cloud Assets API
- Spritesheet packing for `packable` inputs
- Luau code generation referencing uploaded assets
- `init` command to scaffold a `tungsten.toml` configuration
- `test` command to verify upload connectivity with a sample asset
- Lockfile (`tungsten.lock.toml`) caching uploaded asset IDs
- Example spritesheets project under `examples/`
