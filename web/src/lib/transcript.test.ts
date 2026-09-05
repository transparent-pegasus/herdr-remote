import { expect, test } from "vitest";
import {
	agentRawState,
	append,
	type Card,
	following,
	modalContent,
	type Page,
	prepend,
	receivePage,
	replaceTail,
	type Sent,
	sentCard,
	settle,
	type TranscriptState,
} from "./transcript";

const card = (seq: number, role: Card["role"] = "assistant"): Card => ({
	seq,
	role,
	preview: `p${seq}`,
	html: `<p>${seq}</p>`,
});

test("a later live failure keeps raw output painted for the same pane", () => {
	let state = { text: "", hidden: true };
	for (const screen of ["first screen", undefined]) {
		state = agentRawState(screen) ?? state;
	}

	expect(state).toEqual({
		text: "first screen",
		hidden: false,
	});
});

test("append adds only messages the list does not already carry", () => {
	const merged = append([card(1), card(2)], [card(2), card(3)]);
	expect(merged.map((c) => c.seq)).toEqual([1, 2, 3]);
});

test("append returns the same array when an overlapping poll changed nothing", () => {
	const existing = [card(1), card(2)];
	expect(append(existing, [card(1), card(2)])).toBe(existing);
});

test("append keeps the newer copy of a message that was rewritten", () => {
	const existing = [card(1), card(2)];
	const rewritten = { ...card(2), preview: "edited" };
	const merged = append(existing, [rewritten]);
	expect(merged).not.toBe(existing);
	expect(merged[1].preview).toBe("edited");
});

test("replaceTail treats an empty newest window as an empty transcript", () => {
	expect(replaceTail([card(1), card(2)], [])).toEqual([]);
});

test("replaceTail replaces the overlapping tail and keeps loaded history", () => {
	const existing = [card(1), card(2), card(3)];
	const rewritten = { ...card(2), preview: "edited" };
	const replaced = replaceTail(existing, [rewritten, card(3), card(4)]);
	expect(replaced.map((c) => c.seq)).toEqual([1, 2, 3, 4]);
	expect(replaced[1].preview).toBe("edited");
});

test("replaceTail keeps loaded history when the newest window starts after a gap", () => {
	expect(
		replaceTail([card(1), card(2), card(3)], [card(30), card(31)]).map(
			(c) => c.seq,
		),
	).toEqual([1, 2, 3, 30, 31]);
});

test("a new source discards every old card even when its window starts at seq 10", () => {
	const state: TranscriptState = {
		source: "A",
		cards: [card(0), card(1), card(30)],
		sent: [{ after: 30, text: "queued for A" }],
	};
	const messages = [
		{ ...card(10), preview: "B answer", html: "<p>B answer</p>" },
	];
	const page: Page = { source: "B", messages, has_more: true };
	expect(receivePage(state, page)).toEqual({
		source: "B",
		cards: messages,
		sent: [],
	});
});

test("A to unresolved to B drops old sends and preserves input typed while unresolved", () => {
	const state: TranscriptState = {
		source: "A",
		cards: [card(1), card(30)],
		sent: [{ after: 30, text: "queued for A" }],
	};
	const empty: Page = { messages: [], has_more: false };
	const unresolved = receivePage(state, empty);
	expect(unresolved).toEqual({ source: undefined, cards: [], sent: [] });
	const pending = [{ after: -1, text: "typed for B" }];
	const waiting = receivePage({ ...unresolved, sent: pending }, empty);
	expect(waiting).toEqual({ source: undefined, cards: [], sent: pending });
	const messages = [
		{ ...card(10), preview: "B answer", html: "<p>B answer</p>" },
	];
	expect(
		receivePage(waiting, { source: "B", messages, has_more: true }),
	).toEqual({
		source: "B",
		cards: messages,
		sent: pending,
	});
});

test("the same source replaces its tail while preserving loaded history and queued sends", () => {
	const sent = [{ after: 10, text: "still queued" }];
	const state: TranscriptState = {
		source: "A",
		cards: [card(1), card(2), card(10), card(11), card(12)],
		sent,
	};
	const rewritten = {
		...card(10),
		preview: "rewritten",
		html: "<p>rewritten</p>",
	};
	expect(
		receivePage(state, {
			source: "A",
			messages: [rewritten, card(11)],
			has_more: true,
		}),
	).toEqual({
		source: "A",
		cards: [card(1), card(2), rewritten, card(11)],
		sent,
	});
});

test("a same-source arrival settles a queued send only once across repeated pages", () => {
	const state: TranscriptState = {
		source: "A",
		cards: [card(1, "user"), card(10)],
		sent: [
			{ after: 10, text: "first" },
			{ after: 10, text: "second" },
		],
	};
	const messages = [
		{ ...card(11, "user"), preview: "first", html: "<p>first</p>" },
		card(12),
	];
	const page = { source: "A", messages, has_more: true };
	const received = receivePage(state, page);
	expect(received).toEqual({
		source: "A",
		cards: [card(1, "user"), card(10), ...messages],
		sent: [{ after: 11, text: "second" }],
	});
	expect(receivePage(received, page)).toEqual(received);
});

test("the first known source keeps legitimate pending input until its user turn arrives", () => {
	const pending = [{ after: -1, text: "first prompt" }];
	const state: TranscriptState = { cards: [], sent: pending };
	const first = receivePage(state, {
		source: "A",
		messages: [card(0)],
		has_more: false,
	});
	expect(first).toEqual({ source: "A", cards: [card(0)], sent: pending });
	const arrival = {
		...card(1, "user"),
		preview: "first prompt",
		html: "<p>first prompt</p>",
	};
	expect(
		receivePage(first, {
			source: "A",
			messages: [card(0), arrival],
			has_more: false,
		}),
	).toEqual({
		source: "A",
		cards: [card(0), arrival],
		sent: [],
	});
});

test("prepend puts older messages in front, without duplicating the overlap", () => {
	expect(
		prepend([card(1), card(2)], [card(2), card(3)]).map((c) => c.seq),
	).toEqual([1, 2, 3]);
	const existing = [card(2)];
	expect(prepend([], existing)).toBe(existing);
});

test("the modal wraps only what we escaped here, not what the server rendered", () => {
	// Both speakers' words arrive as rendered markdown; a message still waiting
	// for the agent's own file is the one text we escaped ourselves.
	expect(modalContent(card(1, "user"))).toEqual({
		wrap: false,
		html: "<p>1</p>",
	});
	expect(modalContent(sentCard({ after: 1, text: "a\nb" }, 0)).wrap).toBe(true);
});

test("a card whose command output changed is a card that changed", () => {
	const one = card(1, "user");
	const two = { ...one, output: "Set model to Opus 5" };
	expect(append([one], [two])).toEqual([two]);
	expect(append([two], [two])).toEqual([two]);
});

test("the tail is followed only from the tail", () => {
	expect(following(920, 80, 1000)).toBe(true);
	expect(following(0, 80, 1000)).toBe(false);
	// Eight pixels of slop, matching the existing raw-output view.
	expect(following(915, 80, 1000)).toBe(true);
	expect(following(500, 80, 1000)).toBe(false);
});

test("a queued message shows below the transcript, escaped and collapsed", () => {
	const card = sentCard({ after: 12, text: "  a <b>\n\n  two   lines  " }, 0);
	expect(card.role).toBe("user");
	expect(card.seq).toBeGreaterThan(12);
	// The break the sender typed survives; the blank line between does not.
	expect(card.preview).toBe("a <b>\ntwo lines");
	// The modal shows what was typed, whitespace and all; only the preview is
	// collapsed, the way the server collapses its own.
	expect(card.html).toBe("  a &lt;b&gt;\n\n  two   lines  ");
});

const arrived = (seq: number, role: "user" | "assistant" = "user") => ({
	seq,
	role,
	preview: "*bullet*",
	html: "",
});

test("queued messages settle in the order they were sent", () => {
	const sent = [
		{ after: 10, text: "first" },
		{ after: 10, text: "second" },
	];
	expect(settle(sent, [arrived(11)])).toEqual([{ after: 11, text: "second" }]);
	expect(settle(sent, [arrived(11), arrived(12)])).toEqual([]);
});

/** The poll calls it again on every page that changed anything, and a second
 *  call against the same cards used to eat the next queued message. */
test("settling twice against the same turns settles no more than once", () => {
	const sent = [
		{ after: 10, text: "first" },
		{ after: 10, text: "second" },
	];
	const cards = [arrived(11, "assistant"), arrived(12)];
	const once = settle(sent, cards);
	expect(once).toHaveLength(1);
	expect(settle(once, cards)).toBe(once);
	expect(settle(settle(once, cards), cards)).toHaveLength(1);
});

/** A cleared context starts the file's sequence again; a queued message must
 *  not be stranded above a number the transcript will never reach. */
test("a transcript that restarted its sequence does not strand a message", () => {
	const sent = [{ after: 400, text: "queued" }];
	expect(settle(sent, [arrived(0, "assistant")])).toEqual([
		{ after: -1, text: "queued" },
	]);
	expect(settle(sent, [arrived(0, "assistant"), arrived(1)])).toEqual([]);
});

test("a user turn older than the send does not settle it", () => {
	const sent = [{ after: 10, text: "queued" }];
	expect(settle(sent, [arrived(9), arrived(10, "assistant")])).toBe(sent);
});

test("nothing queued returns the same array", () => {
	const empty: Sent[] = [];
	expect(settle(empty, [])).toBe(empty);
});

/** Some agents hand the whole queue over as one turn. */
test("one turn carrying every queued message settles all of them", () => {
	const sent = [
		{ after: 10, text: "* list one\n* list two" },
		{ after: 10, text: "and a third" },
	];
	const together = {
		seq: 11,
		role: "user" as const,
		preview: "list one list two and a third",
		html: "",
	};
	expect(settle(sent, [together])).toEqual([]);
	expect(settle(settle(sent, [together]), [together])).toEqual([]);
});

/** And some hand it over one at a time, which the same turn text decides. */
test("one turn carrying only the first leaves the rest queued", () => {
	const sent = [
		{ after: 10, text: "* list one\n* list two" },
		{ after: 10, text: "and a third" },
	];
	const first = {
		seq: 11,
		role: "user" as const,
		preview: "list one list two",
		html: "",
	};
	expect(settle(sent, [first])).toEqual([{ after: 11, text: "and a third" }]);
});

/** A turn whose text the parser left unrecognisable still settles exactly one. */
test("a turn the text cannot account for settles one message", () => {
	const sent = [
		{ after: 10, text: "[see the docs](https://example.test)" },
		{ after: 10, text: "second" },
	];
	const arrival = {
		seq: 11,
		role: "user" as const,
		preview: "see the docs",
		html: "",
	};
	expect(settle(sent, [arrival])).toEqual([{ after: 11, text: "second" }]);
});
