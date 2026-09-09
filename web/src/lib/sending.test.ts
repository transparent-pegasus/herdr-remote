import { readFileSync } from "node:fs";
import { setImmediate } from "node:timers/promises";
import { runInNewContext } from "node:vm";
import ts from "typescript";
import { expect, test } from "vitest";
import { stopPresentation } from "./api";
import { type Card, type Sent, settle } from "./transcript";

// Execute the page's send handlers with a controllable request and transcript poll.
const page = readFileSync(
	new URL("../pages/index.astro", import.meta.url),
	"utf8",
);
const script = page.split("<script>")[1].split("</script>")[0];
const ast = ts.createSourceFile(
	"page.ts",
	script,
	ts.ScriptTarget.Latest,
	true,
);
const handlers = ast.statements
	.filter(
		(node) =>
			(ts.isFunctionDeclaration(node) &&
				["act", "clearTranscript", "remember"].includes(
					node.name?.text ?? "",
				)) ||
			(ts.isExpressionStatement(node) &&
				["composerEl", "stopEl"].some((element) =>
					node.getText(ast).startsWith(`${element}.addEventListener`),
				)),
	)
	.map((node) => node.getText(ast))
	.join("\n");

const model: Card = {
	seq: 11,
	role: "user",
	preview: "/model",
	html: "<p>/model</p>",
	output: "Set model to Opus 5",
};

function sending() {
	let submit = (_event: { preventDefault: () => void }) => {};
	let clickClear = () => {};
	const forgotten: string[] = [];
	const state = {
		cards: [] as Card[],
		sent: [] as Sent[],
		source: "A" as string | undefined,
		clearedSource: undefined as string | undefined,
		conversation: 0,
		pane: { id: "pane", state: "working" },
		current: () => ({ pane: state.pane }),
		paintCards: () => {},
		transcriptEl: { scrollTop: 0, scrollHeight: 0 },
		moreEl: { hidden: false },
		fullEl: { close: () => {} },
		screenEl: { close: () => {} },
		say: () => {},
		complain: () => {},
		render: () => {},
		settle,
		forgetPane: (paneId: string) => forgotten.push(paneId),
		stopPresentation,
		interruptPane: async (_id: string) => {},
		stopEl: {
			disabled: false,
			addEventListener: (_event: string, handler: typeof clickClear) => {
				clickClear = handler;
			},
		},
		sendEl: { disabled: false },
		textEl: { value: "/model" },
		syncSend: () => {},
		sendPrompt: async (_id: string, _text: string) => {},
		composerEl: {
			addEventListener: (_event: string, handler: typeof submit) => {
				submit = handler;
			},
		},
	};
	runInNewContext(ts.transpile(handlers), state);
	const send = async (request: () => Promise<void>) => {
		state.sendPrompt = request;
		submit({ preventDefault: () => {} });
		await setImmediate();
	};
	const clear = async (request: () => Promise<void>) => {
		state.sendPrompt = request;
		state.pane.state = "idle";
		clickClear();
		await setImmediate();
	};
	return { state, send, clear, forgotten };
}

test("a successful clear immediately retires the visible conversation", async () => {
	const { state, clear, forgotten } = sending();
	state.cards = [model];
	state.sent = [{ after: 11, text: "queued" }];

	await clear(async () => {});

	expect({
		source: state.source,
		clearedSource: state.clearedSource,
		cards: state.cards,
		sent: state.sent,
		conversation: state.conversation,
		moreHidden: state.moreEl.hidden,
		forgotten,
	}).toEqual({
		source: undefined,
		clearedSource: "A",
		cards: [],
		sent: [],
		conversation: 1,
		moreHidden: true,
		forgotten: ["pane"],
	});
});

test("a model command arriving before the send reply leaves no pending copy", async () => {
	const { state, send } = sending();
	await send(async () => {
		state.cards = [model];
	});
	expect(state.cards).toEqual([model]);
	expect(state.sent).toEqual([]);
});

test("a successful send waits on the transcript position from before the request", async () => {
	const { state, send } = sending();
	await send(async () => {
		state.pane.state = "idle";
	});
	expect(state.sent).toEqual([{ after: -1, text: "/model" }]);
	expect(settle(state.sent, [model])).toEqual([]);
});

test("a failed send leaves no pending copy", async () => {
	const { state, send } = sending();
	await send(async () => {
		throw new Error("request failed");
	});
	expect(state.sent).toEqual([]);
});
