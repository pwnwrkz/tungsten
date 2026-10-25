---
title: CLI Reference
description: A comprehensive reference for all Tungsten commands and flags.
---

## `tungsten`

The main entry point for the Tungsten CLI.

### Flags

| Flag              | Description                                           |
| ----------------- | ----------------------------------------------------- |
| `-v`, `--verbose` | Enables verbose logging for troubleshooting (global). |
| `-V`, `--version` | Prints the current version of Tungsten.               |
| `-h`, `--help`    | Prints a list of available commands and their usage.  |

---

## `tungsten init`

Initializes a new `tungsten.toml` configuration file and updates your `.gitignore` file in the current directory.

```bash title="Terminal"
tungsten init
```

:::caution
This command will fail if a `tungsten.toml` already exists in your directory to prevent accidental overwriting of existing configurations.
:::

---

## `tungsten test`

Validates your `tungsten.toml` configuration, verifies your Open Cloud API key, and ensures all assets referenced in your input globs are accessible.

```bash title="Terminal"
tungsten test [flags]
```

### Flags

| Flag        | Value    | Description                                                                         |
| ----------- | -------- | ----------------------------------------------------------------------------------- |
| `--api-key` | `string` | Optional: Provides your Open Cloud API key to verify connection health with Roblox. |

---

## `tungsten sync`

The primary command for syncing your local assets to Roblox and generating project-specific Luau modules.

```bash title="Terminal"
tungsten sync [target] [flags]
```

### Targets

`target` is optional and defaults to `cloud`.

| Target   | Description                                                                                 |
| -------- | ------------------------------------------------------------------------------------------- |
| `cloud`  | Uploads assets directly to the Roblox Open Cloud API (default).                             |
| `studio` | Copies assets into Roblox Studio's content folder for live local preview.                   |
| `debug`  | Copies assets into a `.tungsten_debug/` folder to inspect the final output before upload.   |

### Flags

| Flag        | Value     | Description                                                                     |
| ----------- | --------- | ------------------------------------------------------------------------------- |
| `--api-key` | `string`  | Provides your Open Cloud API key for authentication (required for `cloud`).     |
| `--dry-run` | `boolean` | Simulates the entire sync process without uploading or modifying remote assets. |

:::tip
Use the `--dry-run` flag to safely verify your configuration, packing results, and file structure without consuming your Open Cloud upload quota.
:::

---

## `tungsten watch`

Starts a long-running process that watches your asset folders for changes. When a file is modified, added, or deleted, Tungsten automatically performs a sync.

```bash title="Terminal"
tungsten watch <target> [flags]
```

### Targets

`target` is optional and defaults to `cloud`.

| Target   | Description                                                                                 |
| -------- | ------------------------------------------------------------------------------------------- |
| `cloud`  | Uploads assets directly to the Roblox Open Cloud API.                                       |
| `studio` | Copies assets into Roblox Studio's content folder for live local preview.                   |
| `debug`  | Copies assets into a `.tungsten_debug/` folder to inspect the final output before upload.   |

### Flags

| Flag        | Value    | Description                                                                     |
| ----------- | -------- | ------------------------------------------------------------------------------- |
| `--api-key` | `string` | Provides your Open Cloud API key for authentication (required for `cloud`).     |

---

## `tungsten migrate-lock`

Explicitly migrates a legacy v2 lockfile (`tungsten.lock.toml` or v2 `tungsten.lock`) to the v3 name-keyed format (`tungsten.lock`).

```bash title="Terminal"
tungsten migrate-lock [flags]
```

### Flags

| Flag          | Value     | Description                                                                                   |
| ------------- | --------- | --------------------------------------------------------------------------------------------- |
| `--lockfile`  | `path`    | Path to the lockfile (default: `tungsten.lock`, or `tungsten.lock.toml` if only that exists). |
| `--config`    | `path`    | Path to the configuration file (default: `tungsten.toml`).                                    |
| `--dry-run`   | `boolean` | Computes the migration and prints a summary without writing anything to disk.                 |
| `--force`     | `boolean` | Overwrites an existing v3 lockfile.                                                           |
| `--no-backup` | `boolean` | Skips writing the `.v2.bak` backup file.                                                      |

---

## Environment Variables

| Variable                 | Description                                                                                                    |
| ------------------------ | -------------------------------------------------------------------------------------------------------------- |
| `TUNGSTEN_GLOBAL_APIKEY` | Global API key for CI/CD and automation. Checked if no CLI flag or `.env` key is found.                        |
| `TUNGSTEN_API_KEY`       | Local API key loaded from a `.env` file in the project root. Takes precedence over `TUNGSTEN_GLOBAL_APIKEY`.     |

:::note
API key resolution order:
1. `--api-key` command-line flag
2. `TUNGSTEN_API_KEY` in local `.env`
3. `TUNGSTEN_GLOBAL_APIKEY` system environment variable
:::
