import { readFileSync } from "node:fs";
import { setImmediate } from "node:timers/promises";
import { runInNewContext } from "node:vm";
import ts from "typescript";
import { expect, test } from "vitest";
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
				["act", "remember"].includes(node.name?.text ?? "")) ||
			(ts.isExpressionStatement(node) &&
				node.getText(ast).startsWith("composerEl.addEventListener")),
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
	const state = {
		cards: [] as Card[],
		sent: [] as Sent[],
		conversation: 0,
		pane: { id: "pane", state: "working" },
		current: () => ({ pane: state.pane }),
		paintCards: () => {},
		transcriptEl: { scrollTop: 0, scrollHeight: 0 },
		say: () => {},
		complain: () => {},
		render: () => {},
		settle,
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
	return { state, send };
}

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
