export default (_ctx) => ({
  name: "tauri-dev",
  service: {
    command: "npx",
    args: ["tauri", "dev"],
    group: "tauri",
  },
  dashboard: {
    short: "tdev",
    color: "ylw",
    order: 40,
  },
});
