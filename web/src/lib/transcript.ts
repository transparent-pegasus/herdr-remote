export type Card = {
	seq: number;
	role: "user" | "assistant";
	preview: string;
	html: string;
};

export type Page = { messages: Card[]; has_more: boolean };

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
