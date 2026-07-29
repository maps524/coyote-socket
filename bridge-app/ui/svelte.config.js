import { vitePreprocess } from '@sveltejs/vite-plugin-svelte'

// `svelte-check` looks here rather than at vite.config.ts, so the preprocessor
// has to be declared in both places or type-checking cannot read `lang="ts"`.
export default {
  preprocess: vitePreprocess(),
}
