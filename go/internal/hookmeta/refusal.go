package hookmeta

// Refusal reads whether a hook result refuses what it was fired about,
// and says so the same way for the ledger (decision: "blocked" or
// "denied"), for the fire loop (a refusal stops the later hooks) and
// for every fire site that applies it. They used to disagree: the
// ledger keyed on the key being present, the sites on a string, so
// {deny: true} was recorded "denied" while the call ran, and {deny:
// false} stopped the later hooks without deciding anything.
//
// A string or true refuses; true, or an empty string, carries no
// reason, so the reason is the decision's own word. Anything else
// (false, a number) decides nothing. "block" is read before "deny".
func Refusal(res map[string]any) (decision, reason string) {
	for _, k := range []struct{ key, word string }{{"block", "blocked"}, {"deny", "denied"}} {
		switch v := res[k.key].(type) {
		case string:
			if v == "" {
				v = k.word
			}
			return k.word, v
		case bool:
			if v {
				return k.word, k.word
			}
		}
	}
	return "", ""
}
