import path from "path";

// Use the release-fast profile during development. Drops LTO + uses 16
// codegen-units, which cuts incremental link time from ~3 min to ~30s.
// To cut a real release build, run `cargo build --release` directly from
// src-tauri/ — that still uses the size-optimized [profile.release] config.
const PROFILE = "release-fast";

export default (ctx) => {
  const profileDir = path.join(ctx.SRC_TAURI_DIR, "target", PROFILE);
  const exeName = ctx.IS_WIN ? "coyote-socket.exe" : "coyote-socket";
  const binaryPath = path.join(profileDir, exeName);

  return {
    name: "coyote-bin",
    service: {
      binary: binaryPath,
      shadowCopy: true,
      args: [],
      group: "tauri",
      env: {
        DEV_URL: ctx.VITE_DEV_URL,
      },
    },
    build: {
      command: "cargo",
      args: ["build", "--profile", PROFILE],
      cwd: ctx.SRC_TAURI_DIR,
      service: "coyote-bin",
      aliases: ["coyote", "tauri-bin", "bin"],
    },
    clean: {
      packages: ["coyote-socket"],
      cwd: ctx.SRC_TAURI_DIR,
      service: "coyote-bin",
      safe: true,
      aliases: ["coyote", "tauri"],
    },
    dashboard: {
      short: "bin",
      color: "mag",
      order: 30,
      logFilterKey: "2",
      buildKey: "b",
    },
  };
};
