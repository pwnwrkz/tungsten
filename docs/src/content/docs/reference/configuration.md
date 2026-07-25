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
ts_declaration = true

# Global upload concurrency (optional, default: 10)
max_concurrent_uploads = 10

# Studio-specific configuration (optional)
[studio]
studio_path = "C:/Program Files/Roblox"  # Optional: override auto-detection
auto_route_version = true  # Optional: fetch latest Studio version automatically

# Example: UI Icons
[inputs.icons]
type = "image"
path = "assets/icons/**/*"
output_path = "src/Icons.luau"
packable = true
svg_scale = 2.0
bleed = true  # Optional: enable alpha bleeding (default: true)

# Example: Large backgrounds
[inputs.backgrounds]
type = "image"
path = "assets/backgrounds/**/*"
output_path = "src/Backgrounds.luau"
packable = false
bleed = false  # Disable alpha bleeding for full-frame backgrounds

# Large backgrounds mean a lot of data, so...
# ...compress them!
[inputs.backgrounds.compress_options]
jpeg_quality = 75
png_quality = 50
keep_metadata = false

# Example: Audio and Models
[inputs.audio]
type = "audio"
path = "assets/audio/**/*"
output_path = "src/Audio.luau"

[inputs.models]
type = "model"
path = "assets/models/**/*"
output_path = "src/Models.luau"
```

## Root-level Fields

### `max_concurrent_uploads`

Controls the maximum number of simultaneous uploads to the Roblox Open Cloud API.

| Field                    | Type     | Default | Description                                                                                 |
| ------------------------ | -------- | ------- | ------------------------------------------------------------------------------------------- |
| `max_concurrent_uploads` | `number` | `10`    | Increase for faster uploads on high-bandwidth connections; decrease if hitting rate limits. |

### `[studio]`

Configures Studio sync behavior. All fields are optional.

| Field                | Type      | Description                                                                                                                                                         |
| -------------------- | --------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `studio_path`        | `string`  | Base path to Roblox installation (where `Versions` folder lives). If omitted, Tungsten auto-detects.                                                                |
| `auto_route_version` | `boolean` | If `true` and `studio_path` is set, fetches the latest Studio version from `https://setup.roblox.com/versionQTStudio` and appends `Versions/<version>` to the path. |

---

### `[creator]`

Defines which Roblox account or group assets are uploaded under.

| Field  | Type                  | Description                                                      |
| ------ | --------------------- | ---------------------------------------------------------------- |
| `type` | `"user"` or `"group"` | Whether to upload under a user or a group, defaults to `"user"`. |
| `id`   | `number`              | The Roblox user or group ID to upload under.                     |

---

### `[codegen]`

Controls how Tungsten generates your Luau output files.

| Field             | Type                   | Description                                                               |
| ----------------- | ---------------------- | ------------------------------------------------------------------------- |
| `style`           | `"flat"` or `"nested"` | The structure of the generated Luau table, defaults to `"flat"`.          |
| `strip_extension` | `boolean`              | Whether to strip the file extension from asset keys, defaults to `false`. |
| `ts_declaration`  | `boolean`              | Whether to generate a TypeScript definition file, defaults to `false`.    |

---

### `[inputs.<name>]`

Defines a set of assets to sync. You can define as many input blocks as you need — each one is identified by its name (e.g. `[inputs.packed_assets]`).

| Field         | Type      | Description                                                                                                    |
| ------------- | --------- | -------------------------------------------------------------------------------------------------------------- |
| `path`        | `string`  | A glob pattern pointing to the assets to sync.                                                                 |
| `output_path` | `string`  | Where Tungsten writes the generated Luau file.                                                                 |
| `packable`    | `boolean` | Whether to pack matched assets into a spritesheet before uploading.                                            |
| `svg_scale`   | `number`  | (Optional) Multiplier for SVG rasterization, defaults to 1.0.                                                  |
| `bleed`       | `boolean` | (Optional) Enable alpha bleeding to prevent edge artifacts, defaults to `true`.                                |
| `type`        | `string`  | The Roblox asset type (e.g., `decal`, `image`, `audio`, `model`). Overrides type inferred from file extension. |

:::note
When `packable` is set to `true`, Tungsten packs the matched images into a spritesheet on the fly before uploading. The spritesheet is never saved to disk.
:::

---

### `[inputs.<name>.compress_options]`

Enables and configures image optimization for a specific input group. When this table is present, Tungsten uses `libcaesium` to reduce the file size of your images before they are uploaded to Roblox.

| Field           | Type      | Description                                                           |
| --------------- | --------- | --------------------------------------------------------------------- |
| `jpeg_quality`  | `number`  | Quality of the JPEG image (0-100), defaults to 80.                    |
| `png_quality`   | `number`  | Quality of the PNG image (0-100), defaults to 80.                     |
| `keep_metadata` | `boolean` | Whether to keep metadata in the compressed image, defaults to `true`. |
