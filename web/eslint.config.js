import astro from "eslint-plugin-astro";
import tseslint from "typescript-eslint";

export default tseslint.config(
	{ ignores: ["dist/", ".astro/"] },
	...tseslint.configs.recommended,
	...astro.configs["flat/recommended"],
	{
		rules: {
			"@typescript-eslint/no-explicit-any": "error",
		},
	},
);
