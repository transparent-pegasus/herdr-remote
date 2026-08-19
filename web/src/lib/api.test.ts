import { expect, test } from "vitest";
import { isTransient, type Pane, paneSubtitle } from "./api";

const pane = (over: Partial<Pane> = {}): Pane => ({
	id: "w1:p1",
	label: "orchestrator",
	agent: null,
	state: "unknown",
	...over,
});

test("agent panes show agent and state", () => {
	expect(paneSubtitle(pane({ agent: "claude", state: "working" }))).toBe(
		"claude · working",
	);
});

test("agentless panes read as a plain shell", () => {
	expect(paneSubtitle(pane())).toBe("shell");
});

test("a timed-out request is transient, so the UI retries", () => {
	expect(
		isTransient(new DOMException("signal timed out", "TimeoutError")),
	).toBe(true);
	// fetch() rejects this way when WARP drops mid-request.
	expect(isTransient(new TypeError("Failed to fetch"))).toBe(true);
});

test("a real server error is not transient, so its message survives", () => {
	expect(isTransient(new Error("no such pane"))).toBe(false);
	expect(isTransient(new DOMException("aborted", "AbortError"))).toBe(false);
});
