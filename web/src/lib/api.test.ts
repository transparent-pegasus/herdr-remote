import { expect, test } from "vitest";
import { type Pane, paneSubtitle } from "./api";

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
