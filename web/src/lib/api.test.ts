import { expect, test, vi } from "vitest";
import {
	fetchSession,
	isTransient,
	type Pane,
	paneSubtitle,
	pressArrow,
	stateIndicator,
	stopPresentation,
} from "./api";

const session = {
	workspaces: [
		{
			id: "w2",
			label: "aituber-v1",
			tabs: [
				{
					id: "w2:t1",
					label: "backend",
					panes: [
						{
							id: "w2:p1",
							label: "orchestrator",
							agent: "claude",
							state: "idle",
						},
					],
				},
			],
		},
	],
};

const pane = (over: Partial<Pane> = {}): Pane => ({
	id: "w1:p1",
	label: "orchestrator",
	agent: null,
	state: "unknown",
	...over,
});

test("each defined pane state has its specified indicator", () => {
	expect(stateIndicator(["blocked"])).toBe("yellow");
	expect(stateIndicator(["working"])).toBe("green");
	expect(stateIndicator(["idle"])).toBe("gray");
	expect(stateIndicator(["done"])).toBe("gray");
	expect(stateIndicator(["unknown"])).toBe("transparent");
});

test("aggregate indicators use yellow, green, then gray priority", () => {
	expect(stateIndicator(["idle", "working", "blocked"])).toBe("yellow");
	expect(stateIndicator(["done", "working", "unknown"])).toBe("green");
	expect(stateIndicator(["unknown", "idle", "done"])).toBe("gray");
});

test("aggregate priority is independent of pane order", () => {
	expect(stateIndicator(["blocked", "working", "idle"])).toBe("yellow");
	expect(stateIndicator(["idle", "working", "blocked"])).toBe("yellow");
});

test("an empty state collection is transparent", () => {
	expect(stateIndicator([])).toBe("transparent");
});

test("an unrecognized state is transparent", () => {
	expect(stateIndicator(["future-state"])).toBe("transparent");
});

test("session responses carry workspaces", async () => {
	vi.stubGlobal("fetch", async () => Response.json(session));
	try {
		await expect(fetchSession()).resolves.toEqual(session);
	} finally {
		vi.unstubAllGlobals();
	}
});

test("session responses reject the old top-level tabs", async () => {
	vi.stubGlobal("fetch", async () =>
		Response.json({ tabs: session.workspaces[0].tabs }),
	);
	try {
		await expect(fetchSession()).rejects.toThrow(
			"unexpected response from /api/session",
		);
	} finally {
		vi.unstubAllGlobals();
	}
});

test("agent panes show agent and state", () => {
	expect(paneSubtitle(pane({ agent: "claude", state: "working" }))).toBe(
		"claude · working",
	);
});

test("agentless panes read as a plain shell", () => {
	expect(paneSubtitle(pane())).toBe("shell");
});

test("a working pane presents the stop action with a hand", () => {
	expect(stopPresentation("working")).toEqual({
		label: "Stop",
		icon: "hand",
		interrupts: true,
	});
});

test("a pane that is not working presents clear with an eraser", () => {
	expect(stopPresentation("idle")).toEqual({
		label: "/clear",
		icon: "eraser",
		interrupts: false,
	});
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

test("an arrow key posts to its own route, with the pane id encoded", async () => {
	const calls: [string, string | undefined][] = [];
	vi.stubGlobal("fetch", async (url: string, init?: RequestInit) => {
		calls.push([url, init?.method]);
		return new Response(null, { status: 204 });
	});
	try {
		await pressArrow("w2:p4J", "up");
		await pressArrow("w2:p4J", "down");
	} finally {
		vi.unstubAllGlobals();
	}
	expect(calls).toEqual([
		["/api/panes/w2%3Ap4J/up", "POST"],
		["/api/panes/w2%3Ap4J/down", "POST"],
	]);
});

test("a refused arrow key surfaces the server's own words", async () => {
	vi.stubGlobal(
		"fetch",
		async () => new Response("not an agent pane", { status: 403 }),
	);
	try {
		await expect(pressArrow("w2:p4H", "up")).rejects.toThrow(
			"not an agent pane",
		);
	} finally {
		vi.unstubAllGlobals();
	}
});
