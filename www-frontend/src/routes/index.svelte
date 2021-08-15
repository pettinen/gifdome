<header class="center">
  <h1><a href="https://t.me/GIFdome">@GIFdome</a></h1>
</header>

<!--
<nav class="center">
  Go to:
  <a href="#finished-matches">Finished matches</a>
  <a href="#submissions">Submissions</a>
</nav>

<h2 class="center" id="bracket">Bracket</h2>
<div class="center">
  <a rel="external" href="{baseURL}/static/bracket.png">
    <img style="max-width: 80vw; max-height: 80vh;" src="{baseURL}/static/bracket.png" alt="Bracket">
  </a>
</div>
-->

{#if currentMatch}
  <h2 class="center" id="current-match">Current match</h2>
  <div class="match">
    <Sticker sticker="{stickers[currentMatch.participants[0]]}" />
    <span class="vs">vs</span>
    <Sticker sticker="{stickers[currentMatch.participants[1]]}" />
  </div>
{/if}

{#if finishedMatches.length}
  <h2 class="center" id="finished-matches">Finished matches</h2>
  {#each finishedMatches as [i, match]}
    <h3 class="center">#{i + 1}</h3>
    {#if match.votes}
      <h4 class="center">{match.votes[0]}&ndash;{match.votes[1]}</h4>
    {/if}
    <div class="match">
      <div class:gif-winner="{match.participants[0] === match.winner}">
        <GIF gif="{gifs[match.participants[0]]}" />
      </div>
      <span style="padding: 0 1em;">vs</span>
      <div class:gif-winner="{match.participants[1] === match.winner}">
        <GIF gif="{gifs[match.participants[1]]}" />
      </div>
    </div>
  {/each}
{/if}

<h2 class="center" id="submissions">Submissions</h2>
{#each submissionCounts as count (count)}
<h3>{count} submission{plural(count)}: {gifCount(count)} GIF{plural(gifCount(count))}</h3>
  <div class="gif-showcase">
    {#each submissionsByCount.get(count) as gifID (gifID)}
      <GIF size="12vw" gif="{gifs[gifID]}" />
    {/each}
  </div>
{/each}

<script lang="ts">
import {onMount} from "svelte";

import {plural} from "$lib/utils";
import GIF from "$lib/components/GIF.svelte";


const baseURL = "https://gifdome.dipo.rocks";
const apiURL = `${baseURL}/api/v1`;

let gifs = {};
let matches = [];
let submissions = {};

$: currentMatch = matches.find(match => !match.winner) || null
$: finishedMatches = matches.filter(match => match.winner !== null).reverse();
$: submissionCounts = [...submissionsByCount.keys()].sort((a, b) => b - a);
$: submissionsByCount = (() => {
  const out = new Map<number, string[]>();
  for (const [gifID, count] of Object.entries(submissions)) {
    if (!out.has(count))
      out.set(count, []);
    out.get(count)?.push(gifID);
  }
  return out;
})();
$: gifCount = submissionCount => submissionsByCount.get(submissionCount)?.length ?? 0;

/*
async function fetchMatches() {
  const response = await fetch(`${apiURL}/matches.json`);
  matches = await response.json();
}
*/

async function fetchGIFs() {
  const response = await fetch(`${apiURL}/gifs.json`);
  gifs = await response.json();
}

async function fetchSubmissions() {
  const response = await fetch(`${apiURL}/submissions.json`);
  submissions = await response.json();
  for (const [gifID, count] of Object.entries(submissions)) {
    if (gifs[gifID])
      gifs[gifID].submissions = count;
  }
}

onMount(async () => {
  await fetchGIFs();
  await fetchSubmissions();
  //await fetchMatches();
});
</script>

<style lang="scss">
  @import "../app.css";

  * {
    font-family: sans-serif;
  }

  h4 {
    margin: 0;
  }

  nav a {
    margin-left: .5ex;
  }

  .match {
    margin: 1ex 0;
    display: flex;
    justify-content: center;
    align-items: center;
  }

  .vs {
    margin: 0 1em;
  }

  .gif-showcase {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    grid-template-rows: 12vw;
  }

  @media (min-width: 768px) {
    .gif-showcase { grid-template-columns: repeat(8, 1fr); }
  }

  .gif-winner {
    border: 3px solid gold;
  }

  .center {
    display: flex;
    justify-content: center;
  }
</style>
