import { expect, test } from "vitest";
import {
	agentRawState,
	append,
	type Card,
	following,
	modalContent,
	prepend,
	type Sent,
	sentCard,
	settle,
	replaceTail,
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

test("prepend puts older messages in front, without duplicating the overlap", () => {
	expect(
		prepend([card(1), card(2)], [card(2), card(3)]).map((c) => c.seq),
	).toEqual([1, 2, 3]);
	const existing = [card(2)];
	expect(prepend([], existing)).toBe(existing);
});

test("the modal wraps a user's text and renders an agent's markdown", () => {
	expect(modalContent(card(1, "user"))).toEqual({
		wrap: true,
		html: "<p>1</p>",
	});
	expect(modalContent(card(1, "assistant"))).toEqual({
		wrap: false,
		html: "<p>1</p>",
	});
});

test("the tail is followed only from the tail", () => {
	expect(following(920, 80, 1000)).toBe(true);
	expect(following(0, 80, 1000)).toBe(false);
	// Eight pixels of slop, matching the existing raw-output view.
	expect(following(915, 80, 1000)).toBe(true);
	expect(following(500, 80, 1000)).toBe(false);
});

test("a queued message shows below the transcript, escaped and collapsed", () => {
	const card = sentCard({ after: 12, text: "  a <b>\n  two lines  " }, 0);
	expect(card.role).toBe("user");
	expect(card.seq).toBeGreaterThan(12);
	expect(card.preview).toBe("a <b> two lines");
	// The modal shows what was typed, whitespace and all; only the preview is
	// collapsed, the way the server collapses its own.
	expect(card.html).toBe("  a &lt;b&gt;\n  two lines  ");
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
