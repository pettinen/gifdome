import nodeAdapter from "@sveltejs/adapter-node";
import preprocess from "svelte-preprocess";


export default {
  preprocess: preprocess(),
  kit: {
    adapter: nodeAdapter({precompress: true}),
  },
};
