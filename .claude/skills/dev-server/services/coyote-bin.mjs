export default (ctx) => ({
  name: "coyote-bin",
  service: {
    binary: ctx.bin("coyote-socket"),
    shadowCopy: true,
    args: [],
    group: "tauri",
    env: {
      DEV_URL: ctx.VITE_DEV_URL,
    },
  },
  build: {
    command: "cargo",
    args: ["build", "--release"],
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
});
