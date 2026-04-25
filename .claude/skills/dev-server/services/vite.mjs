export default (ctx) => ({
  name: "vite",
  service: {
    command: "npx",
    args: ["vite"],
    healthUrl: ctx.VITE_DEV_URL,
  },
  dashboard: {
    short: "vite",
    color: "grn",
    order: 10,
    logFilterKey: "1",
  },
});
