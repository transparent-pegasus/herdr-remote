export type Card = {
	seq: number;
	role: "user" | "assistant";
	preview: string;
	html: string;
};

export type Page = { messages: Card[]; has_more: boolean };

export function agentRawState(screen?: string):
	| {
			text: string;
			hidden: boolean;
	  }
	| undefined {
	return screen === undefined ? undefined : { text: screen, hidden: false };
}

const same = (a: Card, b: Card): boolean =>
	a.role === b.role && a.preview === b.preview && a.html === b.html;

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
	};
}

/** A queued message settles when a user turn newer than the transcript it was
 *  sent against appears; the agent takes them in the order they were sent, so
 *  the oldest unsettled one is the one that arrived. Matching on the text
 *  cannot work — the card carries the markdown parser's rendering of it, and a
 *  message that was a list or had emphasis comes back without its syntax.
 *
 *  Called on every poll that changed anything, so it must answer the same way
 *  twice: what settles a message is recorded by moving the next one's `after`
 *  past the turn just claimed, not by consuming it. */
export function settle(sent: Sent[], cards: Card[]): Sent[] {
	if (sent.length === 0 || cards.length === 0) return sent;
	const newest = cards[cards.length - 1].seq;
	const turns = cards.filter((card) => card.role === "user");
	const rest: Sent[] = [];
	let claimed = -1;
	for (const one of sent) {
		// A transcript whose newest message is older than the one this was sent
		// against has started its sequence again — a cleared context, a new
		// session file — so the message is waiting on everything, not on a
		// number the file will never reach.
		const after = Math.max(newest < one.after ? -1 : one.after, claimed);
		const arrival = turns.find((card) => card.seq > after);
		if (arrival) {
			claimed = arrival.seq;
			continue;
		}
		rest.push(after === one.after ? one : { ...one, after });
	}
	const same =
		rest.length === sent.length && rest.every((one, at) => one === sent[at]);
	return same ? sent : rest;
}

/** The agent's half is markdown the server rendered; the user's half is text
 *  the server escaped. Both arrive as HTML, and only the user's needs the
 *  whitespace of what they actually typed. */
export const modalContent = (card: Card): { wrap: boolean; html: string } => ({
	wrap: card.role === "user",
	html: card.html,
});

/** Follow the tail only when the reader is already at it. The eight pixels of
 *  slop are the same the raw-output view uses. */
export const following = (
	scrollTop: number,
	clientHeight: number,
	scrollHeight: number,
): boolean => scrollTop + clientHeight >= scrollHeight - 8;
