---
title: CLI Reference
description: A comprehensive reference for all Tungsten commands and flags.
---

## `tungsten`

The main entry point for the Tungsten CLI.

### Flags

| Flag              | Description                                          |
| ----------------- | ---------------------------------------------------- |
| `-V`, `--version` | Prints the current version of Tungsten.              |
| `-H`, `--help`    | Prints a list of available commands and their usage. |

---

## `tungsten init`

Initializes a new `tungsten.toml` configuration file and updates your `.gitignore` file in the current directory.

```bash title="Terminal"
tungsten init
```

:::caution
This command will fail if a `tungsten.toml` already exists in your directory to prevent accidental overwriting of existing configurations.
:::

## `tungsten test`

Validates your tungsten.toml configuration and ensures all assets referenced in your input globs are accessible.

```bash title="Terminal"
tungsten test --api-key <key>
```

### Flags

| Flag        | Value    | Description                                                                         |
| ----------- | -------- | ----------------------------------------------------------------------------------- |
| `--api-key` | `string` | Optional: Provides your Open Cloud API key to verify connection health with Roblox. |

## `tungsten sync`

The primary command for syncing your local assets to Roblox and generating project-specific Luau modules.

```bash title="Terminal"
tungsten sync <target> [flags]
```

### Targets

| Target   | Description                                                                                 |
| -------- | ------------------------------------------------------------------------------------------- |
| `cloud`  | Uploads assets directly to the Roblox Open Cloud API.                                       |
| `studio` | Syncs assets to your local Roblox Studio instance.                                          |
| `debug`  | Creates a `.tungsten_debug` folder, allowing you to inspect the final output before upload. |

### Flags

| Flag        | Value     | Description                                                                     |
| ----------- | --------- | ------------------------------------------------------------------------------- |
| `--api-key` | `string`  | Provides your Open Cloud API key for authentication.                            |
| `--dry-run` | `boolean` | Simulates the entire sync process without uploading or modifying remote assets. |

:::tip
Use the `--dry-run` flag to safely verify your configuration and packing results without consuming your Open Cloud upload quota.
:::

## `tungsten watch`

Starts a long-running process that watches your assets folder for changes. When a file is modified, added, or deleted, Tungsten automatically performs a sync.

```bash title="Terminal"
tungsten watch <target> [flags]
```

### Targets

| Target   | Description                                                                                 |
| -------- | ------------------------------------------------------------------------------------------- |
| `cloud`  | Uploads assets directly to the Roblox Open Cloud API.                                       |
| `studio` | Syncs assets to your local Roblox Studio instance.                                          |
| `debug`  | Creates a `.tungsten_debug` folder, allowing you to inspect the final output before upload. |

### Flags

| Flag        | Value    | Description                                          |
| ----------- | -------- | ---------------------------------------------------- |
| `--api-key` | `string` | Provides your Open Cloud API key for authentication. |
