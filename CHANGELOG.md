# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [v3.0.0-rc.1] - 2026-06-05

### Added

- CHANGELOG.md file to track changes between versions
- Added asset_type override to uploads, allowing specifying asset type (e.g., decal, image) independent of file kind.

### Changed

- Improved variable naming in `src/core/assets/img/alpha_bleed.rs` for better readability:
  - Replaced single-letter variables (`w`, `h`, `len`, `i`, `x`, `y`) with descriptive names (`width`, `height`, `pixel_count`, `index`, `x`, `y`)
  - Improved clarity in BFS algorithm with more descriptive variable names (`red_sum`, `green_sum`, `blue_sum`, `sample_count`)
  - Renamed queue variables for clarity (`current` -> `current_wave`, `next` -> `next_wave`)
- Updated ignore reason in `src/core/assets/img/convert.rs` test:
  - Added descriptive reason to ignored test: `#[ignore = "TGA support not fully tested in CI environment"]`
- Improved documentation accuracy:
  - Fixed creator configuration example in docs/getting-started/first-sync.mdx to show correct [creator] format
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
