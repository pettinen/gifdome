<script lang="ts">
	export let id: string;

	let video: HTMLVideoElement;

	$: src = `/gifdome/animations/${id}`;
	$: poster = `/gifdome/thumbnails/${id}`;
    // TODO: fix autoplay error in chrome
	$: video?.addEventListener('mouseover', () => { video?.play(); });
</script>

<!-- svelte-ignore a11y-media-has-caption -->
<div>
	<video bind:this={video} autoplay loop {poster}>
		<source {src} type="video/mp4" />
	</video>
	<p>{id}</p>
</div>

<style lang="scss">
	div {
		display: inline-flex;
		flex-direction: column;
		align-items: center;
		margin: 0 0.5rem;
	}
	video {
		height: 10rem;
		padding: 0.5rem;
	}
	p {
		margin: 0;
	}
</style>
