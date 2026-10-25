---
title: Configuration Reference
description: A reference for every field in tungsten.toml.
---

Tungsten is configured through a `tungsten.toml` file in your project's root directory. Run `tungsten init` to generate one, or create it manually.

## Full example

```toml title="tungsten.toml"
[creator]
type = "user"
id = 12345678

[codegen]
style = "nested"
strip_extension = true
strip_whitespace = false
ts_declaration = true

# Global upload concurrency (optional, default: 10)
max_concurrent_uploads = 10

# Studio-specific configuration (optional)
[studio]
studio_path = "C:/Program Files/Roblox"  # Optional: override auto-detection
auto_route_version = true  # Optional: fetch latest Studio version automatically

# Example: UI Icons with uniform cell spritesheet packing
[inputs.icons]
path = "assets/icons/**/*"
output_path = "src/Icons.luau"
packable = true
uniform_cells = true
cell_size = 64        # or rect: cell_size = [64, 32]
cell_fit = "contain"  # "contain", "cover", or "stretch"
svg_scale = 2.0
bleed = true          # Optional: enable alpha bleeding (default: true)

# Example: Large backgrounds
[inputs.backgrounds]
path = "assets/backgrounds/**/*"
output_path = "src/Backgrounds.luau"
packable = false
bleed = false  # Disable alpha bleeding for full-frame backgrounds

# Compress images with libcaesium
[inputs.backgrounds.compress_options]
jpeg_quality = 75
png_quality = 50
keep_metadata = false

# Example: Audio transcoding & loudness normalization
[inputs.sounds]
path = "assets/sounds/**/*"
output_path = "src/Sounds.luau"

# Transcode to OGG Vorbis
[inputs.sounds.transcode]
vorbis_quality = 5.0  # Quality scale -1.0 to 10.0 (or specify bitrate = 160)

# EBU R128 loudness normalization
[inputs.sounds.normalize]
target_lufs = -14.0
true_peak_ceiling = -1.0

# Example: Models
[inputs.models]
path = "assets/models/**/*"
output_path = "src/Models.luau"

# Example: Pre-uploaded Roblox web assets
[inputs.icons.web]
"assets/icons/legacy_badge.png" = { id = 1234567890 }
```

## Root-level Fields

### `max_concurrent_uploads`

Controls the maximum number of simultaneous uploads to the Roblox Open Cloud API.

| Field                    | Type     | Default | Description                                                                                 |
| ------------------------ | -------- | ------- | ------------------------------------------------------------------------------------------- |
| `max_concurrent_uploads` | `number` | `10`    | Increase for faster uploads on high-bandwidth connections; decrease if hitting rate limits. |

---

### `[studio]`

Configures Studio sync behavior. All fields are optional.

| Field                | Type      | Default | Description                                                                                                                                                         |
| -------------------- | --------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `studio_path`        | `string`  | *auto*  | Base path to Roblox installation (where `Versions` folder lives). If omitted, Tungsten auto-detects.                                                                |
| `auto_route_version` | `boolean` | `false` | If `true` and `studio_path` is set, fetches the latest Studio version from `https://setup.roblox.com/versionQTStudio` and appends `Versions/<version>` to the path. |

---

### `[creator]`

Defines which Roblox account or group assets are uploaded under.

| Field  | Type                  | Default  | Description                                                      |
| ------ | --------------------- | -------- | ---------------------------------------------------------------- |
| `type` | `"user"` or `"group"` | `"user"` | Whether to upload under a user or a group.                       |
| `id`   | `number`              | *none*   | The Roblox user or group ID to upload under (required).          |

---

### `[codegen]`

Controls how Tungsten generates your Luau output files.

| Field              | Type                   | Default   | Description                                                               |
| ------------------ | ---------------------- | --------- | ------------------------------------------------------------------------- |
| `style`            | `"flat"` or `"nested"` | `"flat"`  | The structure of the generated Luau table.                                |
| `strip_extension`  | `boolean`              | `false`   | Whether to strip the file extension from asset keys.                       |
| `strip_whitespace` | `boolean`              | `false`   | Whether to remove whitespace from asset keys.                             |
| `ts_declaration`   | `boolean`              | `false`   | Whether to generate a TypeScript definition file (`.d.ts`).               |

---

### `[inputs.<name>]`

Defines a set of assets to sync. You can define as many input blocks as you need — each one is identified by its name (e.g. `[inputs.icons]`).

| Field           | Type                     | Default     | Description                                                                                                                                 |
| --------------- | ------------------------ | ----------- | ------------------------------------------------------------------------------------------------------------------------------------------- |
| `path`          | `string`                 | *required*  | A glob pattern pointing to the assets to sync.                                                                                              |
| `output_path`   | `string`                 | *required*  | Where Tungsten writes the generated Luau file.                                                                                              |
| `type`          | `string`                 | *auto*      | Overrides the Roblox asset type (e.g., `"decal"`, `"audio"`, `"model"`). If omitted, inferred from file format or structure.              |
| `packable`      | `boolean`                | `false`     | Whether to pack matched images/SVGs into a spritesheet before uploading.                                                                   |
| `uniform_cells` | `boolean`                | `true`      | When `packable = true`, pack onto a uniform cell grid rather than bin-packing.                                                              |
| `cell_size`     | `number` or `[u32, u32]` | *auto*      | Square cell dimension (e.g. `64`) or rectangular `[width, height]` for uniform packing. Defaults to max sprite dimensions across the input.|
| `cell_fit`      | `string`                 | `"contain"` | Resampling fit policy when sprite dimensions differ: `"contain"`, `"cover"`, or `"stretch"`.                                                |
| `svg_scale`     | `number`                 | `1.0`       | Multiplier for SVG rasterization. Can also be overridden per directory or asset in `.tmeta` files.                                          |
| `bleed`         | `boolean`                | `true`      | Enable alpha bleeding to prevent edge discoloration/artifacts on transparent images.                                                        |

:::note
When `packable` is set to `true`, Tungsten packs the matched images into a spritesheet on the fly before uploading. Spritesheets are cached in `tungsten.lock` and are never saved to disk.
:::

---

### `[inputs.<name>.compress_options]`

Enables and configures image compression for a specific input group via `libcaesium`. All fields are optional when the table is present.

| Field           | Type      | Default | Description                                                                        |
| --------------- | --------- | ------- | ---------------------------------------------------------------------------------- |
| `jpeg_quality`  | `number`  | `80`    | Quality of JPEG images (1–100).                                                    |
| `png_quality`   | `number`  | `80`    | Quality of PNG images (1–100).                                                     |
| `keep_metadata` | `boolean` | `false` | Whether to preserve EXIF/XMP/ICC metadata in the compressed image.                 |

---

### `[inputs.<name>.transcode]`

Enables OGG Vorbis transcoding for audio files in the input group (MP3, WAV, FLAC, M4A, etc.). Sources that are already OGG are passed through untouched unless normalization is enabled.

| Field            | Type     | Default | Description                                                                                   |
| ---------------- | -------- | ------- | --------------------------------------------------------------------------------------------- |
| `vorbis_quality` | `number` | `5.0`   | Vorbis VBR quality on the `-1.0` to `10.0` scale (5.0 is ~160 kbps stereo).                   |
| `bitrate`        | `number` | *none*  | Target VBR bitrate in kbps (e.g. `128`, `192`). Takes precedence over `vorbis_quality`.        |

---

### `[inputs.<name>.normalize]`

Enables EBU R128 / ITU-R BS.1770-4 integrated loudness normalization. Enabling this table automatically decodes and re-encodes audio to OGG Vorbis (even existing OGG files).

| Field               | Type     | Default | Description                                                                                   |
| ------------------- | -------- | ------- | --------------------------------------------------------------------------------------------- |
| `target_lufs`       | `number` | `-14.0` | Integrated loudness target in LUFS (standard streaming loudness target).                      |
| `true_peak_ceiling` | `number` | `-1.0`  | True peak ceiling in dBTP. Gain is limited so output never exceeds this ceiling.              |

---

### `[inputs.<name>.web]`

Maps local relative asset paths to existing Roblox asset IDs without re-uploading them.

```toml
[inputs.icons.web]
"assets/icons/badge.png" = { id = 1234567890 }
```

| Field | Type     | Description                                           |
| ----- | -------- | ----------------------------------------------------- |
| `id`  | `number` | The existing Roblox asset ID to emit in the codegen.  |

---

## Asset Metadata Sidecars (`.tmeta`)

In addition to `tungsten.toml`, individual assets or directories can be configured with `.tmeta` TOML sidecar files.

### Priority

1. Asset-specific file sidecar: `<name>.<ext>.tmeta` (e.g. `icon.png.tmeta`)
2. Fallback asset sidecar: `<name>.tmeta` (e.g. `icon.tmeta`)
3. Nearest directory sidecar: `<dir>.tmeta` (applies to nested files)
4. Configured `tungsten.toml` defaults

### Supported Sidecar Fields

| Field         | Type     | Description                                                                                           |
| ------------- | -------- | ----------------------------------------------------------------------------------------------------- |
| `name`        | `string` | Overrides the display name sent to Roblox.                                                            |
| `description` | `string` | Overrides the description sent to Roblox.                                                             |
| `type`        | `string` | Overrides the asset type sent to Roblox. `"auto"` derives it from the file kind. For `.rbxm`/`.rbxmx` files, `"animation"` or `"model"` pins the type without parsing DOM. Takes precedence over the input-level `type`. |
| `svg_scale`   | `number` | Overrides the rasterization scale multiplier for SVGs in this file or directory.                      |
