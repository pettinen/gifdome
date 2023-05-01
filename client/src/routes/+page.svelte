<script lang="ts">
	import Animation from '$lib/components/Animation.svelte';

	import 'normalize.css';
	import '@fontsource/fira-sans/latin.css';
	import 'material-icons/iconfont/material-icons.css';
	import '$lib/styles/main.scss';

	const get_duplicate_suggestions = async () => {
		const res = await fetch('/gifdome/api/duplicates/suggestions?tournament=@GIFdome');
		return res.json();
	};

	const duplicate_suggestions: Promise<string[][]> = get_duplicate_suggestions();
</script>

{#await duplicate_suggestions}
	<p>loading...</p>
{:then suggestion_lists}
	<h1>Possible duplicates</h1>
	{#each suggestion_lists as suggestions}
		<div>
			{#each suggestions as id}
				<Animation {id} />
			{/each}
		</div>
	{/each}
{:catch}
	<p>oops</p>
{/await}
