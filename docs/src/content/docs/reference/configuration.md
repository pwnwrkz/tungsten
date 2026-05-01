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

# Example: UI Icons
[inputs.icons]
path = "assets/icons/**/*"
output_path = "src/Icons.luau"
packable = true
svg_scale = 2.0

# Example: Large backgrounds
[inputs.backgrounds]
path = "assets/backgrounds/**/*"
output_path = "src/Backgrounds.luau"
packable = false

# Large backgrounds mean a lot of data, so...
# ...compress them!
[inputs.backgrounds.compress_options]
jpeg_quality = 75
png_quality = 50
keep_metadata = false

# Example: Audio and Models
[inputs.audio]
path = "assets/audio/**/*"
output_path = "src/Audio.luau"

[inputs.models]
path = "assets/models/**/*"
output_path = "src/Models.luau"
```

## Fields

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

| Field         | Type      | Description                                                         |
| ------------- | --------- | ------------------------------------------------------------------- |
| `path`        | `string`  | A glob pattern pointing to the assets to sync.                      |
| `output_path` | `string`  | Where Tungsten writes the generated Luau file.                      |
| `packable`    | `boolean` | Whether to pack matched assets into a spritesheet before uploading. |
| `svg_scale`   | `number`  | (Optional) Multiplier for SVG rasterization, defaults to 1.0.       |

:::note
When `packable` is set to `true`, Tungsten packs the matched images into a spritesheet on the fly before uploading. The spritesheet is never saved to disk.
:::

### `[inputs.<name>.compress_options]`

Enables and configures image optimization for a specific input group. When this table is present, Tungsten uses `libcaesium` to reduce the file size of your images before they are uploaded to Roblox.

| Field           | Type      | Description                                                           |
| --------------- | --------- | --------------------------------------------------------------------- |
| `jpeg_quality`  | `number`  | Quality of the JPEG image (0-100), defaults to 80.                    |
| `png_quality`   | `number`  | Quality of the PNG image (0-100), defaults to 80.                     |
| `keep_metadata` | `boolean` | Whether to keep metadata in the compressed image, defaults to `true`. |
