package ui

// The bough mark as terminal art, for the empty-session welcome.
//
// GENERATED from assets/logo-1024.png in the Rust TUI (commit c36b5a64):
// the icon point-sampled onto a character grid through the ` .:-=+*#`
// density ramp — ASCII, not block pixels, so the log's rings and the
// sprout read as structure rather than a smeared photo. Cells darker
// than the icon's black plate are blank, so the mark sits on the
// terminal's own background in any theme; each glyph keeps its source
// colour, so the sprout stays green and the log tan.

import "strings"

// markCols / markSmallCols are the display columns each art occupies.
const (
	markCols      = 16
	markSmallCols = 9
)

// markArt is the mark at full size: 16 columns, 24 rows.
var markArt = []string{
	"\x1b[0m         \x1b[0m\x1b[38;2;32;67;53m.\x1b[0m\x1b[0m",
	"\x1b[0m         \x1b[0m\x1b[38;2;78;201;143m--\x1b[0m  \x1b[0m\x1b[38;2;80;207;147m-\x1b[0m\x1b[0m",
	"\x1b[0m         \x1b[0m\x1b[38;2;78;201;143m--\x1b[0m\x1b[38;2;74;191;136m-\x1b[0m\x1b[38;2;78;201;143m--\x1b[0m\x1b[0m",
	"\x1b[0m          \x1b[0m\x1b[38;2;78;201;143m--\x1b[0m\x1b[38;2;81;209;148m-\x1b[0m\x1b[0m",
	"\x1b[0m         \x1b[0m\x1b[38;2;78;201;143m--\x1b[0m\x1b[0m",
	"\x1b[0m        \x1b[0m\x1b[38;2;78;201;143m--\x1b[0m\x1b[0m",
	"\x1b[0m        \x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[38;2;80;207;147m-\x1b[0m\x1b[0m",
	"\x1b[0m       \x1b[0m\x1b[38;2;81;209;148m-\x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[0m",
	"\x1b[0m  \x1b[0m\x1b[38;2;47;46;41m.\x1b[0m\x1b[38;2;238;219;166m+\x1b[0m\x1b[38;2;202;168;87m=\x1b[0m\x1b[38;2;194;156;69m--\x1b[0m\x1b[38;2;78;201;143m--\x1b[0m\x1b[38;2;194;156;69m-\x1b[0m\x1b[38;2;193;155;67m-\x1b[0m\x1b[38;2;206;173;96m=\x1b[0m\x1b[38;2;243;223;168m+\x1b[0m\x1b[0m",
	"\x1b[0m \x1b[0m\x1b[38;2;236;217;163m+\x1b[0m\x1b[38;2;193;155;67m-\x1b[0m\x1b[38;2;225;201;139m=\x1b[0m\x1b[38;2;192;154;65m-\x1b[0m\x1b[38;2;193;154;66m-\x1b[0m\x1b[38;2;212;181;108m=\x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[38;2;76;202;144m-\x1b[0m\x1b[38;2;213;184;112m=\x1b[0m\x1b[38;2;193;154;66m-\x1b[0m\x1b[38;2;194;155;68m-\x1b[0m\x1b[38;2;199;163;80m=\x1b[0m\x1b[38;2;192;153;65m-\x1b[0m\x1b[38;2;236;217;163m+\x1b[0m\x1b[0m",
	"\x1b[0m\x1b[38;2;236;217;163m+\x1b[0m\x1b[38;2;192;153;65m-\x1b[0m\x1b[38;2;236;217;163m+\x1b[0m\x1b[38;2;193;155;67m-\x1b[0m\x1b[38;2;237;218;164m+\x1b[0m\x1b[38;2;238;220;167m+\x1b[0m\x1b[38;2;192;153;64m-\x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[38;2;80;200;142m-\x1b[0m\x1b[38;2;200;164;81m=\x1b[0m\x1b[38;2;197;161;76m-\x1b[0m\x1b[38;2;236;217;163m+\x1b[0m\x1b[38;2;192;153;65m-\x1b[0m\x1b[38;2;237;218;165m+\x1b[0m\x1b[38;2;233;214;158m+\x1b[0m\x1b[38;2;236;217;163m+\x1b[0m",
	"\x1b[0m\x1b[38;2;235;215;159m+\x1b[0m\x1b[38;2;236;216;162m+\x1b[0m\x1b[38;2;193;154;66m-\x1b[0m\x1b[38;2;208;177;101m=\x1b[0m\x1b[38;2;195;158;73m-\x1b[0m\x1b[38;2;194;156;69m-\x1b[0m\x1b[38;2;192;153;65m-\x1b[0m\x1b[38;2;212;182;109m=\x1b[0m\x1b[38;2;207;175;98m=\x1b[0m\x1b[38;2;192;154;66m-\x1b[0m\x1b[38;2;223;198;133m=\x1b[0m\x1b[38;2;193;154;66m-\x1b[0m\x1b[38;2;237;219;166m+\x1b[0m\x1b[38;2;193;155;66m-\x1b[0m\x1b[38;2;236;217;163m+\x1b[0m\x1b[38;2;220;194;128m=\x1b[0m",
	"\x1b[0m\x1b[38;2;217;180;95m=\x1b[0m\x1b[38;2;216;179;93m=\x1b[0m\x1b[38;2;237;218;165m+\x1b[0m\x1b[38;2;203;168;89m=\x1b[0m\x1b[38;2;194;156;69m-\x1b[0m\x1b[38;2;194;156;70m-\x1b[0m\x1b[38;2;206;173;95m=\x1b[0m\x1b[38;2;199;164;81m=\x1b[0m\x1b[38;2;201;166;84m=\x1b[0m\x1b[38;2;205;173;94m=\x1b[0m\x1b[38;2;192;154;65m-\x1b[0m\x1b[38;2;194;156;69m-\x1b[0m\x1b[38;2;227;203;142m=\x1b[0m\x1b[38;2;238;220;167m+\x1b[0m\x1b[38;2;194;156;69m--\x1b[0m",
	"\x1b[0m\x1b[38;2;217;180;95m====\x1b[0m\x1b[38;2;217;179;94m=\x1b[0m\x1b[38;2;220;186;106m=\x1b[0m\x1b[38;2;229;204;140m=\x1b[0m\x1b[38;2;235;214;157m+\x1b[0m\x1b[38;2;234;213;155m+\x1b[0m\x1b[38;2;229;203;137m=\x1b[0m\x1b[38;2;218;181;97m=\x1b[0m\x1b[38;2;217;180;94m=\x1b[0m\x1b[38;2;194;156;69m----\x1b[0m",
	"\x1b[0m\x1b[38;2;217;180;95m============\x1b[0m\x1b[38;2;194;156;69m----\x1b[0m",
	"\x1b[0m\x1b[38;2;217;180;95m============\x1b[0m\x1b[38;2;194;156;69m----\x1b[0m",
	"\x1b[0m\x1b[38;2;217;180;95m============\x1b[0m\x1b[38;2;194;156;69m----\x1b[0m",
	"\x1b[0m\x1b[38;2;217;180;95m============\x1b[0m\x1b[38;2;194;156;69m----\x1b[0m",
	"\x1b[0m\x1b[38;2;217;180;95m============\x1b[0m\x1b[38;2;194;156;69m----\x1b[0m",
	"\x1b[0m\x1b[38;2;217;180;95m============\x1b[0m\x1b[38;2;194;156;69m----\x1b[0m",
	"\x1b[0m\x1b[38;2;217;180;95m============\x1b[0m\x1b[38;2;194;156;69m----\x1b[0m",
	"\x1b[0m\x1b[38;2;217;180;95m============\x1b[0m\x1b[38;2;194;156;69m---\x1b[0m\x1b[38;2;195;156;69m-\x1b[0m",
	"\x1b[0m \x1b[0m\x1b[38;2;217;180;95m===========\x1b[0m\x1b[38;2;194;156;69m---\x1b[0m\x1b[0m",
	"\x1b[0m   \x1b[0m\x1b[38;2;221;183;97m=\x1b[0m\x1b[38;2;217;180;95m======\x1b[0m\x1b[38;2;210;172;86m=\x1b[0m\x1b[38;2;194;156;69m-\x1b[0m\x1b[38;2;189;152;68m-\x1b[0m\x1b[0m",
}

// markArtSmall is the same mark for a shorter pane: 9 columns, 15 rows.
var markArtSmall = []string{
	"\x1b[0m     \x1b[0m\x1b[38;2;70;176;126m-\x1b[0m\x1b[0m",
	"\x1b[0m     \x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[38;2;26;52;43m.\x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[0m",
	"\x1b[0m     \x1b[0m\x1b[38;2;79;203;144m-\x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[0m",
	"\x1b[0m    \x1b[0m\x1b[38;2;36;80;62m.\x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[0m",
	"\x1b[0m    \x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[0m",
	"\x1b[0m \x1b[0m\x1b[38;2;237;219;166m+\x1b[0m\x1b[38;2;194;156;69m-\x1b[0m\x1b[38;2;196;159;73m-\x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[38;2;193;155;67m-\x1b[0m\x1b[38;2;193;154;66m-\x1b[0m\x1b[38;2;244;224;168m+\x1b[0m\x1b[0m",
	"\x1b[0m\x1b[38;2;199;163;80m=\x1b[0m\x1b[38;2;223;197;132m=\x1b[0m\x1b[38;2;227;204;142m=\x1b[0m\x1b[38;2;237;219;166m+\x1b[0m\x1b[38;2;78;201;143m-\x1b[0m\x1b[38;2;231;210;151m+\x1b[0m\x1b[38;2;237;219;165m+\x1b[0m\x1b[38;2;236;217;163m++\x1b[0m",
	"\x1b[0m\x1b[38;2;236;217;163m+\x1b[0m\x1b[38;2;192;154;65m-\x1b[0m\x1b[38;2;193;155;67m-\x1b[0m\x1b[38;2;208;176;100m=\x1b[0m\x1b[38;2;199;163;80m=\x1b[0m\x1b[38;2;198;162;78m-\x1b[0m\x1b[38;2;202;168;87m=\x1b[0m\x1b[38;2;194;156;69m-\x1b[0m\x1b[38;2;238;220;167m+\x1b[0m",
	"\x1b[0m\x1b[38;2;217;180;95m==\x1b[0m\x1b[38;2;216;178;92m=\x1b[0m\x1b[38;2;229;204;140m=\x1b[0m\x1b[38;2;235;214;158m+\x1b[0m\x1b[38;2;225;196;124m=\x1b[0m\x1b[38;2;217;180;94m=\x1b[0m\x1b[38;2;194;156;69m--\x1b[0m",
	"\x1b[0m\x1b[38;2;217;180;95m=======\x1b[0m\x1b[38;2;194;156;69m--\x1b[0m",
	"\x1b[0m\x1b[38;2;217;180;95m=======\x1b[0m\x1b[38;2;194;156;69m--\x1b[0m",
	"\x1b[0m\x1b[38;2;217;180;95m=======\x1b[0m\x1b[38;2;194;156;69m--\x1b[0m",
	"\x1b[0m\x1b[38;2;217;180;95m=======\x1b[0m\x1b[38;2;194;156;69m--\x1b[0m",
	"\x1b[0m\x1b[38;2;217;180;95m=======\x1b[0m\x1b[38;2;194;156;69m--\x1b[0m",
	"\x1b[0m \x1b[0m\x1b[38;2;217;180;95m=====\x1b[0m\x1b[38;2;194;156;69m-\x1b[0m\x1b[38;2;196;157;69m-\x1b[0m\x1b[0m",
}

// mark returns the biggest mark that fits width x height (rows after
// the art are reserved by the caller via spare), centred horizontally;
// nil when neither fits. The art is the first thing dropped: a mark
// clipped in half reads as a rendering bug, while the wording alone
// reads as a deliberately quiet screen.
func mark(width, height, spare int) []string {
	art, cols := markArt, markCols
	if height < len(art)+spare || width < cols+2 {
		art, cols = markArtSmall, markSmallCols
		if height < len(art)+spare || width < cols+2 {
			return nil
		}
	}
	pad := strings.Repeat(" ", (width-cols)/2)
	out := make([]string, len(art))
	for i, row := range art {
		out[i] = pad + row
	}
	return out
}
