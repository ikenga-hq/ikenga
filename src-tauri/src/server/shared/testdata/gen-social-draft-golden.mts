// Generates social-draft-golden.json: the sender's own derivation and validation
// of a set of social posts, which `social_draft.rs` must reproduce (plans/outbox-redesign
// D-15, G-CONTRACT r2 "Proof"). Every expected value below comes from running the
// sender's real `lib/social-draft.ts` (royalti-co `scripts/cron`), never from this script.
//
// Regenerate (Node 22, from the shell repo root):
//
//   <royalti-co>/scripts/cron/node_modules/.bin/tsx \
//     src-tauri/src/server/shared/testdata/gen-social-draft-golden.mts \
//     <royalti-co>/scripts/cron/lib/social-draft.ts \
//     > src-tauri/src/server/shared/testdata/social-draft-golden.json
//
// Each `build` case records `stable(buildSocialItem(canon, draftId))`, using the sender's
// own `stable` key sort (copied verbatim below; the sender does not export it). Each
// `validate` case records the problem codes of `validateSocialPost(canon)`, in order. Each
// `edit` case is a draft built by the sender, a G-CONTRACT r2 patch, and the item the
// sender builds from the post the patch describes (the patch → post mapping is the
// contract's, applied here in plain TypeScript; the derivation is the sender's).

import { pathToFileURL } from 'node:url';

const senderPath = process.argv[2];
if (!senderPath) {
	console.error('usage: gen-social-draft-golden.mts <royalti-co>/scripts/cron/lib/social-draft.ts');
	process.exit(2);
}
const sd = await import(pathToFileURL(senderPath).href);

type Post = {
	platform: string;
	account: string;
	content: string;
	media_url: string | null;
	scheduled_for: string | null;
};

/** The sender's `stable` (lib/social-draft.ts), verbatim. */
function stable(v: unknown): string {
	if (Array.isArray(v)) return `[${v.map(stable).join(',')}]`;
	if (v !== null && typeof v === 'object') {
		const o = v as Record<string, unknown>;
		return `{${Object.keys(o)
			.filter((k) => o[k] !== undefined)
			.sort()
			.map((k) => `${JSON.stringify(k)}:${stable(o[k])}`)
			.join(',')}}`;
	}
	return JSON.stringify(v) ?? 'null';
}

const post = (p: Partial<Post> & { platform: string; content: string }): Post => ({
	account: 'royalti-io',
	media_url: null,
	scheduled_for: null,
	...p,
});

const IMG = 'https://cdn.royalti.io/social/launch.png';
const IMG2 = 'https://cdn.royalti.io/social/second.jpg';
const AT = '2099-03-04T09:30:00.000Z';
const AT2 = '2099-05-06T17:45:00.000Z';
const EMOJI_280 = `${'a'.repeat(278)}😀`; // 278 + 2 UTF-16 units = 280

const buildCases: Array<{ name: string; draftId: string; canon: Post }> = [
	{
		name: 'linkedin, comment, image, scheduled',
		draftId: 'social-post:q-li-1',
		canon: post({
			platform: 'linkedin',
			content: 'We shipped royalty splits.\n\n---firstcomment---\nhttps://royalti.io/blog #MusicBiz',
			media_url: IMG,
			scheduled_for: AT,
		}),
	},
	{
		name: 'linkedin, no comment, no image, unscheduled',
		draftId: 'social-post:q-li-2',
		canon: post({ platform: 'linkedin', content: 'Plain post, nothing else.' }),
	},
	{
		name: 'linkedin personal, comment (padded, mixed-case marker), no image, unscheduled',
		draftId: 'social-post:q-li-3',
		canon: post({
			platform: 'linkedin',
			account: 'personal',
			content: 'Body line one\nBody line two   \n\n  ---FirstComment---  \n\n  the comment\n  second line  \n',
		}),
	},
	{
		name: 'linkedin company, no comment, image, scheduled',
		draftId: 'social-post:q-li-4',
		canon: post({
			platform: 'linkedin',
			account: 'company',
			content: 'Image-only announcement',
			media_url: IMG2,
			scheduled_for: AT2,
		}),
	},
	{
		name: 'x single post (platform "x"), unscheduled',
		draftId: 'social-post:q-x-1',
		canon: post({ platform: 'x', content: 'Short and sweet on X.' }),
	},
	{
		name: 'twitter single post with image and emoji at exactly 280 UTF-16 units',
		draftId: 'social-post:q-x-2',
		canon: post({ platform: 'twitter', content: EMOJI_280, media_url: IMG, scheduled_for: AT }),
	},
	{
		name: 'x thread with an image on the first tweet (derived, not editable)',
		draftId: 'social-post:q-x-3',
		canon: post({
			platform: 'x',
			content: 'Tweet one\n\n---\n\nTweet two\n\n---\n\nTweet three',
			media_url: IMG,
		}),
	},
	{
		name: 'instagram with image and a ---hashtags--- comment',
		draftId: 'social-post:q-ig-1',
		canon: post({
			platform: 'instagram',
			content: 'Studio day ✨\n---hashtags---\n#studio #royalti',
			media_url: IMG,
			scheduled_for: AT,
		}),
	},
	{
		name: 'instagram with image, no comment',
		draftId: 'social-post:q-ig-2',
		canon: post({ platform: 'instagram', content: 'Just the picture.', media_url: IMG2 }),
	},
];

const validateCases: Array<{ name: string; canon: Post }> = [
	{ name: 'valid linkedin', canon: post({ platform: 'linkedin', content: 'ok' }) },
	{
		name: 'x with a first comment',
		canon: post({ platform: 'x', content: 'body\n---firstcomment---\ncomment' }),
	},
	{ name: 'x over 280', canon: post({ platform: 'x', content: 'b'.repeat(281) }) },
	{ name: 'x 279 + emoji is over 280', canon: post({ platform: 'x', content: `${'b'.repeat(279)}😀` }) },
	{ name: 'x exactly 280 with emoji', canon: post({ platform: 'x', content: EMOJI_280 }) },
	{ name: 'x ---tweet--- delimiter', canon: post({ platform: 'twitter', content: 'one ---TWEET--- two' }) },
	{
		name: 'x thread with an empty and an over-long tweet',
		canon: post({ platform: 'x', content: `one\n\n---\n\n   \n\n---\n\n${'c'.repeat(300)}` }),
	},
	{ name: 'http media', canon: post({ platform: 'linkedin', content: 'x', media_url: 'http://a.b/c.png' }) },
	{ name: 'media with a space', canon: post({ platform: 'linkedin', content: 'x', media_url: 'https://a.b/c d.png' }) },
	{ name: 'media padded', canon: post({ platform: 'linkedin', content: 'x', media_url: ' https://a.b/c.png' }) },
	{ name: 'media blank', canon: post({ platform: 'linkedin', content: 'x', media_url: '' }) },
	{ name: 'media not a url', canon: post({ platform: 'linkedin', content: 'x', media_url: 'nota url' }) },
	{ name: 'media not a url, no space', canon: post({ platform: 'linkedin', content: 'x', media_url: 'cdn.royalti.io/x.png' }) },
	{ name: 'media credentials', canon: post({ platform: 'linkedin', content: 'x', media_url: 'https://u:p@a.b/c.png' }) },
	{ name: 'media too long', canon: post({ platform: 'linkedin', content: 'x', media_url: `https://a.b/${'p'.repeat(2040)}` }) },
	{ name: 'empty content', canon: post({ platform: 'linkedin', content: ' \n\t ' }) },
	{ name: 'empty body before the marker', canon: post({ platform: 'linkedin', content: '  \n---firstcomment---\nc' }) },
	{ name: 'marker with nothing after it', canon: post({ platform: 'linkedin', content: 'b\n---firstcomment---\n  \n' }) },
	{
		name: 'two markers',
		canon: post({ platform: 'linkedin', content: 'b\n---firstcomment---\nc\n---hashtags---\n#d' }),
	},
	{ name: 'unknown channel', canon: post({ platform: 'linkedin', account: 'nobody', content: 'b' }) },
	{ name: 'BOM-only content is empty', canon: post({ platform: 'linkedin', content: '﻿' }) },
];

type Patch = {
	body?: string;
	firstComment?: string | null;
	media?: { kind: string; imageUrls: string[] };
	scheduledIso?: string | null;
};

/** The G-CONTRACT r2 mapping of a patch onto `item.social` (what `update` applies). */
function applyPatch(canon: Post, patch: Patch): Post {
	const next = { ...canon };
	if ('body' in patch || 'firstComment' in patch) {
		const cur = sd.splitContent(canon.content);
		const body = 'body' in patch ? (patch.body as string) : cur.body;
		const comment = 'firstComment' in patch ? (patch.firstComment ?? '') : (cur.firstComment ?? '');
		next.content = comment.trim() !== '' ? `${body}\n---firstcomment---\n${comment}` : body;
	}
	if ('media' in patch) next.media_url = patch.media?.imageUrls[0] ?? null;
	if ('scheduledIso' in patch) {
		next.scheduled_for = patch.scheduledIso === null ? null : new Date(Date.parse(patch.scheduledIso as string)).toISOString();
	}
	return next;
}

const editCases: Array<{ name: string; draftId: string; before: Post; patch: Patch }> = [
	{
		name: 'linkedin: new body and comment, image swapped, rescheduled with an offset',
		draftId: 'social-post:q-e-1',
		before: buildCases[0].canon,
		patch: {
			body: 'We shipped royalty splits, now with payouts.',
			firstComment: 'Read more: https://royalti.io/blog/splits',
			media: { kind: 'single', imageUrls: [IMG2] },
			scheduledIso: '2099-03-05T10:00:00+01:00',
		},
	},
	{
		name: 'linkedin: comment removed, image removed, unscheduled',
		draftId: 'social-post:q-e-2',
		before: buildCases[0].canon,
		patch: { firstComment: '', media: { kind: 'single', imageUrls: [] }, scheduledIso: null },
	},
	{
		name: 'linkedin: comment added to a plain post, body kept',
		draftId: 'social-post:q-e-3',
		before: buildCases[1].canon,
		patch: { firstComment: '#royalti #musicbusiness' },
	},
	{
		name: 'x single: body only',
		draftId: 'social-post:q-e-4',
		before: buildCases[4].canon,
		patch: { body: 'Edited on X 😀' },
	},
	{
		name: 'instagram: hashtags comment rewritten under the firstcomment marker, schedule only fractional',
		draftId: 'social-post:q-e-5',
		before: buildCases[7].canon,
		patch: { firstComment: '#studio #newmusic', scheduledIso: '2099-03-04T09:30:00.123456Z' },
	},
];

const out = {
	_generated:
		'tsx src-tauri/src/server/shared/testdata/gen-social-draft-golden.mts <royalti-co>/scripts/cron/lib/social-draft.ts (see the script header)',
	build: buildCases.map((c) => ({
		name: c.name,
		draftId: c.draftId,
		channelId: sd.resolveSocialChannelId(c.canon.platform, c.canon.account),
		canon: c.canon,
		stable: stable(sd.buildSocialItem(c.canon, c.draftId)),
	})),
	validate: validateCases.map((c) => ({
		name: c.name,
		channelKnown: sd.resolveSocialChannelId(c.canon.platform, c.canon.account) !== null,
		canon: c.canon,
		codes: sd.validateSocialPost(c.canon).map((p: { code: string }) => p.code),
	})),
	edit: editCases.map((c) => {
		const after = applyPatch(c.before, c.patch);
		return {
			name: c.name,
			draftId: c.draftId,
			before: sd.buildSocialItem(c.before, c.draftId),
			patch: c.patch,
			after: after,
			stable: stable(sd.buildSocialItem(after, c.draftId)),
		};
	}),
};

process.stdout.write(`${JSON.stringify(out, null, 2)}\n`);
