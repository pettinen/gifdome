<div>
  <video {style} title="{description}">
    <source src="{baseURL}/{gif.file}{ext(gif.mime_type)}" type="{gif.mime_type}">
  </video>
</div>

<script lang="ts">
  import {ext, plural} from "$lib/utils";

  interface GIF {
    id: string;
    file: string;
    filenames: string[];
    mime_type: string;
    duration: number;
    width: number;
    height: number;
    submissions: number;
  }

  export let size = "200px";
  export let gif: GIF;

  $: style = `width: ${size}; height: ${size};`
  $: description = `GIF with ${gif.submissions} submission${plural(gif.submissions)}`;
  const baseURL = "https://gifdome.dipo.rocks/static/gifs";
</script>

<style>
  video {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: contain;
    object-position: center;
  }
</style>
