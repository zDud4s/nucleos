import { mergeConfig } from "vite";
import base from "../../vite.config.js";

export default async (env) =>
  mergeConfig(await (typeof base === "function" ? base(env) : base), {
    root: new URL("../..", import.meta.url).pathname.slice(1),
    resolve: { preserveSymlinks: true },
  });
