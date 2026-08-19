import { expect, test } from "vitest";
import { href, parseRoute } from "./route";

test("the three real shapes round-trip", () => {
	for (const route of [{}, { tabId: "w1" }, { tabId: "w1", paneId: "w1:p2" }]) {
		expect(parseRoute(href(route))).toEqual(route);
	}
});

test("pane ids keep their colons through encoding", () => {
	expect(href({ tabId: "w1", paneId: "w1:p2" })).toBe("/t/w1/p/w1%3Ap2");
});

test("anything unrecognised falls back to the root", () => {
	for (const path of [
		"/",
		"/nope",
		"/t",
		"/t/w1/x/p2",
		"/t/w1/p",
		"/%E0%A4%A",
	]) {
		expect(parseRoute(path).paneId).toBeUndefined();
	}
	expect(parseRoute("/t/w1/x/p2")).toEqual({ tabId: "w1" });
	expect(parseRoute("/t/w1/p")).toEqual({ tabId: "w1" });
});
