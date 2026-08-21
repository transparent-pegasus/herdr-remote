import { expect, test } from "vitest";
import { href, parseRoute } from "./route";

test("the four real shapes round-trip", () => {
	for (const route of [
		{},
		{ workspaceId: "w1" },
		{ workspaceId: "w1", tabId: "w1:t1" },
		{ workspaceId: "w1", tabId: "w1:t1", paneId: "w1:p2" },
	]) {
		expect(parseRoute(href(route))).toEqual(route);
	}
});

test("ids keep their colons through encoding", () => {
	expect(href({ workspaceId: "w1", tabId: "w1:t1", paneId: "w1:p2" })).toBe(
		"/w/w1/t/w1%3At1/p/w1%3Ap2",
	);
});

test("anything unrecognised falls back to the nearest real level", () => {
	expect(parseRoute("/")).toEqual({});
	expect(parseRoute("/nope")).toEqual({});
	expect(parseRoute("/w")).toEqual({});
	expect(parseRoute("/%E0%A4%A")).toEqual({});
	// A level that does not parse drops the levels below it, never the one above.
	expect(parseRoute("/w/w1/x/t1")).toEqual({ workspaceId: "w1" });
	expect(parseRoute("/w/w1/t")).toEqual({ workspaceId: "w1" });
	expect(parseRoute("/w/w1/t/w1%3At1/x/p2")).toEqual({
		workspaceId: "w1",
		tabId: "w1:t1",
	});
	expect(parseRoute("/w/w1/t/w1%3At1/p")).toEqual({
		workspaceId: "w1",
		tabId: "w1:t1",
	});
	// A tab without its workspace is not addressable at all.
	expect(parseRoute("/t/w1%3At1")).toEqual({});
});
