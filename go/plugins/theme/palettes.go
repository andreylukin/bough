package theme

// Bundled palettes. Every palette sets every ui theme token (see
// plugins/ui/theme.go: fg[:bg][:bold|italic|faint], hex or ANSI-256)
// plus "markdown" (the glamour style). The roles are the same across
// palettes so switching never changes what a color MEANS:
//
//	user    the person's line              accent  bough's mark, live cursor
//	focus   the block-cursor highlight     error   failures
//	dim     headers, hints, dividers       border  box edges
//	status  the status bar                 select  the palette's selected row
//	code    executed code text             system  "/" command output
//
// Names match maki's where the palette is the same upstream scheme.
var palettes = map[string]map[string]string{
	// forest is bough's own: the Everforest dark palette (Sainnhe
	// Park). Moss green for the person, aqua for bough, amber focus,
	// grey-green dims on a deep spruce ground.
	"forest": {
		"user":      "#a7c080:bold",
		"assistant": "",
		"code":      "#9da9a0",
		"result":    "",
		"error":     "#e67e80",
		"accent":    "#83c092",
		"dim":       "#859289",
		"border":    "#56635f",
		"status":    "#d3c6aa:#3d484d",
		"focus":     "#dbbc7f:bold",
		"select":    "#d3c6aa:#475258",
		"system":    "#9da9a0",
		"markdown":  "dark",
	},
	// forest_light: Everforest light, for a pale terminal.
	"forest_light": {
		"user":      "#8da101:bold",
		"assistant": "",
		"code":      "#829181",
		"result":    "",
		"error":     "#f85552",
		"accent":    "#35a77c",
		"dim":       "#939f91",
		"border":    "#e0dcc7",
		"status":    "#5c6a72:#efebd4",
		"focus":     "#dfa000:bold",
		"select":    "#5c6a72:#e6e2cc",
		"system":    "#829181",
		"markdown":  "light",
	},
	// ansi: the terminal's own 16 colors, so its scheme shows through
	// (what bough used before palettes).
	"ansi": {
		"user":      "5:bold",
		"assistant": "",
		"code":      "252:faint",
		"result":    "",
		"error":     "1",
		"accent":    "6",
		"dim":       "245",
		"border":    "245",
		"status":    "250:236",
		"focus":     "6:bold",
		"select":    "254:237",
		"system":    "245",
		"markdown":  "",
	},
	"tokyonight": {
		"user":      "#9ece6a:bold",
		"assistant": "",
		"code":      "#a9b1d6",
		"result":    "",
		"error":     "#f7768e",
		"accent":    "#7dcfff",
		"dim":       "#565f89",
		"border":    "#3b4261",
		"status":    "#c0caf5:#292e42",
		"focus":     "#e0af68:bold",
		"select":    "#c0caf5:#3b4261",
		"system":    "#737aa2",
		"markdown":  "dark",
	},
	"catppuccin_mocha": {
		"user":      "#a6e3a1:bold",
		"assistant": "",
		"code":      "#bac2de",
		"result":    "",
		"error":     "#f38ba8",
		"accent":    "#94e2d5",
		"dim":       "#6c7086",
		"border":    "#45475a",
		"status":    "#cdd6f4:#313244",
		"focus":     "#f9e2af:bold",
		"select":    "#cdd6f4:#45475a",
		"system":    "#a6adc8",
		"markdown":  "dark",
	},
	"gruvbox": {
		"user":      "#b8bb26:bold",
		"assistant": "",
		"code":      "#d5c4a1",
		"result":    "",
		"error":     "#fb4934",
		"accent":    "#8ec07c",
		"dim":       "#928374",
		"border":    "#504945",
		"status":    "#ebdbb2:#3c3836",
		"focus":     "#fabd2f:bold",
		"select":    "#ebdbb2:#504945",
		"system":    "#a89984",
		"markdown":  "dark",
	},
	"nord": {
		"user":      "#a3be8c:bold",
		"assistant": "",
		"code":      "#d8dee9",
		"result":    "",
		"error":     "#bf616a",
		"accent":    "#88c0d0",
		"dim":       "#7b88a1",
		"border":    "#4c566a",
		"status":    "#e5e9f0:#3b4252",
		"focus":     "#ebcb8b:bold",
		"select":    "#eceff4:#434c5e",
		"system":    "#8892a8",
		"markdown":  "dark",
	},
	"dracula": {
		"user":      "#50fa7b:bold",
		"assistant": "",
		"code":      "#f8f8f2:faint",
		"result":    "",
		"error":     "#ff5555",
		"accent":    "#8be9fd",
		"dim":       "#6272a4",
		"border":    "#44475a",
		"status":    "#f8f8f2:#44475a",
		"focus":     "#f1fa8c:bold",
		"select":    "#f8f8f2:#6272a4",
		"system":    "#8b95c0",
		"markdown":  "dark",
	},
}
