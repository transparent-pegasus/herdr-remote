export type Card = {
	seq: number;
	role: "user" | "assistant";
	preview: string;
	html: string;
	/** A local command's own output, shown under the command it answers. */
	output?: string;
	/** Text this page escaped rather than markdown the server rendered, which
	 *  is only ever a message still waiting for the agent's own file. */
	wrap?: true;
};

export type Page = {
	messages: Card[];
	has_more: boolean;
	/** Opaque source identity from the response header; absent before resolution. */
	source?: string;
};

export type TranscriptState = { source?: string; cards: Card[]; sent: Sent[] };

export function agentRawState(screen?: string):
	| {
			text: string;
			hidden: boolean;
	  }
	| undefined {
	return screen === undefined ? undefined : { text: screen, hidden: false };
}

const same = (a: Card, b: Card): boolean =>
	a.role === b.role &&
	a.preview === b.preview &&
	a.html === b.html &&
	a.output === b.output;

/** Merge by `seq`, newest copy winning, oldest first. Polls overlap by design —
 *  the server answers with a window, not a delta — so the common case is a page
 *  that changes nothing, and that case returns the original array. */
function merge(existing: Card[], incoming: Card[]): Card[] {
	if (incoming.length === 0) return existing;
	const bySeq = new Map(existing.map((card) => [card.seq, card]));
	let changed = false;
	for (const card of incoming) {
		const seen = bySeq.get(card.seq);
		if (seen && same(seen, card)) continue;
		bySeq.set(card.seq, card);
		changed = true;
	}
	if (!changed) return existing;
	return [...bySeq.values()].sort((a, b) => a.seq - b.seq);
}

export const append = (existing: Card[], incoming: Card[]): Card[] =>
	merge(existing, incoming);

export const prepend = (older: Card[], existing: Card[]): Card[] =>
	merge(existing, older);

/** A newest-window response is authoritative from its first sequence onward.
 * Keep pages already loaded before that boundary and replace everything newer;
 * an empty response means the transcript itself is empty. */
export function replaceTail(existing: Card[], incoming: Card[]): Card[] {
	if (incoming.length === 0) return existing.length === 0 ? existing : [];
	const start = incoming[0].seq;
	const replaced = [
		...existing.filter((card) => card.seq < start),
		...incoming,
	];
	const unchanged =
		replaced.length === existing.length &&
		replaced.every(
			(card, index) =>
				card.seq === existing[index].seq && same(card, existing[index]),
		);
	return unchanged ? existing : replaced;
}

export type Sent = { after: number; text: string };

const escapeText = (text: string): string =>
	text
		.replaceAll("&", "&amp;")
		.replaceAll("<", "&lt;")
		.replaceAll(">", "&gt;")
		.replaceAll('"', "&quot;");

/** A message sent mid-turn is queued by the agent and reaches its own file only
 *  when the agent takes it, which can be minutes. Until then it is shown from
 *  the copy we already have, below everything the file holds — which is where
 *  it will land, after the answer the agent is still writing. The preview is
 *  collapsed and capped the way the server collapses and caps its own. */
export function sentCard(sent: Sent, index: number): Card {
	return {
		seq: Number.MAX_SAFE_INTEGER - index,
		role: "user",
		preview: sent.text.replace(/\s+/g, " ").trim().slice(0, 300),
		html: escapeText(sent.text),
		wrap: true,
	};
}

/** Letters and digits alone. A user's preview is what they typed, collapsed,
 *  and so is ours — but only one of the two went through the server, so the
 *  comparison drops everything whitespace and punctuation could differ on. */
const key = (text: string): string =>
	text.replace(/[^\p{L}\p{N}]/gu, "").toLowerCase();

/** How many queued messages one arriving turn accounts for. An agent may hand
 *  a queue over one turn at a time or fold the whole of it into a single turn,
 *  and that turn's own text is the only thing that says which. Zero means the
 *  text settled nothing — a message past the preview's 300-character cap
 *  cannot match at all — and the caller falls back to one. */
function covered(preview: string, queued: Sent[]): number {
	let left = key(preview);
	let taken = 0;
	while (taken < queued.length) {
		const one = key(queued[taken].text);
		if (one === "" || !left.startsWith(one)) break;
		left = left.slice(one.length);
		taken += 1;
	}
	return taken;
}

/** A queued message settles when a user turn newer than the transcript it was
 *  sent against appears; agents take them in the order they were sent, so the
 *  oldest unsettled one is the one that arrived.
 *
 *  Called on every poll that changed anything, so it must answer the same way
 *  twice: what settles a message is recorded by moving the next one's `after`
 *  past the turn just claimed, not by consuming that turn. */
export function settle(sent: Sent[], cards: Card[]): Sent[] {
	if (sent.length === 0 || cards.length === 0) return sent;
	const newest = cards[cards.length - 1].seq;
	const turns = cards.filter((card) => card.role === "user");
	let rest = sent;
	let claimed = -1;
	for (let head = rest[0]; head !== undefined; head = rest[0]) {
		// A transcript whose newest message is older than the one this was sent
		// against has started its sequence again — a cleared context, a new
		// session file — so the message waits on everything, not on a number
		// the file will never reach.
		const after = Math.max(newest < head.after ? -1 : head.after, claimed);
		const arrival = turns.find((card) => card.seq > after);
		if (arrival === undefined) {
			if (after !== head.after) rest = [{ ...head, after }, ...rest.slice(1)];
			break;
		}
		claimed = arrival.seq;
		rest = rest.slice(Math.max(covered(arrival.preview, rest), 1));
	}
	const same =
		rest.length === sent.length && rest.every((one, at) => one === sent[at]);
	return same ? sent : rest;
}

/** A source change replaces the conversation, including loaded earlier pages.
 *  Input typed while awaiting a source still belongs to the incoming session. */
export function receivePage(
	state: TranscriptState,
	page: Page,
): TranscriptState {
	const changed = state.source !== page.source;
	const cards = replaceTail(changed ? [] : state.cards, page.messages);
	const sent = changed && state.source ? [] : state.sent;
	return { source: page.source, cards, sent: settle(sent, cards) };
}

/** Both speakers' halves are markdown the server rendered. Only a message this
 *  page is still holding for the agent is text it escaped itself, and only
 *  that needs the whitespace of what was actually typed. */
export const modalContent = (card: Card): { wrap: boolean; html: string } => ({
	wrap: card.wrap === true,
	html: card.html,
});

/** Follow the tail only when the reader is already at it. The eight pixels of
 *  slop are the same the raw-output view uses. */
export const following = (
	scrollTop: number,
	clientHeight: number,
	scrollHeight: number,
): boolean => scrollTop + clientHeight >= scrollHeight - 8;
