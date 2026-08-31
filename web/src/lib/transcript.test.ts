import { expect, test } from "vitest";
import {
	append,
	type Card,
	following,
	modalContent,
	prepend,
} from "./transcript";

const card = (seq: number, role: Card["role"] = "assistant"): Card => ({
	seq,
	role,
	preview: `p${seq}`,
	html: `<p>${seq}</p>`,
});

test("append adds only messages the list does not already carry", () => {
	const merged = append([card(1), card(2)], [card(2), card(3)]);
	expect(merged.map((c) => c.seq)).toEqual([1, 2, 3]);
});

test("append returns the same array when the poll changed nothing", () => {
	const existing = [card(1), card(2)];
	expect(append(existing, [card(1), card(2)])).toBe(existing);
	expect(append(existing, [])).toBe(existing);
});

test("append keeps the newer copy of a message that was rewritten", () => {
	const existing = [card(1), card(2)];
	const rewritten = { ...card(2), preview: "edited" };
	const merged = append(existing, [rewritten]);
	expect(merged).not.toBe(existing);
	expect(merged[1].preview).toBe("edited");
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
