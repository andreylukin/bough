package hookmeta

import "testing"

func TestDescription(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct{ name, body, want string }{
		{"empty", "", ""},
		{"plain", "// Description: Explain this.\nreturn {};", "Explain this."},
		{"whitespace", "\ufeff \t\r\n// filename\n\n \t//  Description:  café  \r\nreturn {};", "café"},
		{"eof", "// Description: at EOF", "at EOF"},
		{"first", "// Description: first\n// Description: second", "first"},
		{"empty first", "// Description: \n// Description: second", ""},
		{"case", "// description: nope", ""},
		{"other comment", "// prose Description: nope", ""},
		{"block", "/* license\n// Description: hidden\n*/\n// Description: visible", "visible"},
		{"unterminated block", "/*\n// Description: hidden", ""},
		{"block only", "/* Description: hidden */", ""},
		{"code after block", "/* license */ run();\n// Description: hidden", ""},
		{"after code", "return {};\n// Description: hidden", ""},
		{"inline", "run(); // Description: hidden", ""},
		{"string", "'// Description: hidden';", ""},
		{"template", "`\n// Description: hidden\n`", ""},
		{"no execution", "// Description: safe\nthrow new Error('must not run');", "safe"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			if got := Description(tc.body); got != tc.want {
				t.Fatalf("Description(%q) = %q, want %q", tc.body, got, tc.want)
			}
		})
	}
}
