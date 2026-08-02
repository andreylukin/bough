"""The grading suite. Never present in the workspace; copied in by verify.sh."""

import unittest

from dsv import ParseError, parse


class TestBasics(unittest.TestCase):
    def test_header_and_one_record(self):
        self.assertEqual(parse("a,b\n1,x\n"), [{"a": 1, "b": "x"}])

    def test_custom_delimiter(self):
        self.assertEqual(parse("a|b\n1|x", delimiter="|"), [{"a": 1, "b": "x"}])

    def test_empty_input(self):
        self.assertEqual(parse(""), [])
        self.assertEqual(parse("\n\n  \n"), [])

    def test_header_only(self):
        self.assertEqual(parse("a,b\n"), [])

    def test_no_trailing_newline(self):
        self.assertEqual(parse("a\n1"), [{"a": 1}])


class TestQuoting(unittest.TestCase):
    def test_delimiter_inside_quotes(self):
        self.assertEqual(parse('a,b\n"x,y",2'), [{"a": "x,y", "b": 2}])

    def test_escaped_quote(self):
        self.assertEqual(parse('a\n"he said ""hi"""'), [{"a": 'he said "hi"'}])

    def test_newline_inside_quotes(self):
        self.assertEqual(parse('a,b\n"one\ntwo",3'), [{"a": "one\ntwo", "b": 3}])

    def test_unterminated_quote(self):
        with self.assertRaises(ParseError):
            parse('a\n"oops')

    def test_text_after_closing_quote(self):
        with self.assertRaises(ParseError):
            parse('a\n"x"y')


class TestWhitespace(unittest.TestCase):
    def test_unquoted_is_stripped(self):
        self.assertEqual(parse("a,b\n  x  ,\t2\t"), [{"a": "x", "b": 2}])

    def test_quoted_is_not_stripped(self):
        self.assertEqual(parse('a\n" x "'), [{"a": " x "}])

    def test_blank_lines_anywhere(self):
        self.assertEqual(parse("\n\na,b\n\n1,x\n \n2,y\n"),
                         [{"a": 1, "b": "x"}, {"a": 2, "b": "y"}])


class TestArity(unittest.TestCase):
    def test_too_many_fields(self):
        with self.assertRaises(ParseError):
            parse("a,b\n1,2,3")

    def test_too_few_fields_pad_with_none(self):
        self.assertEqual(parse("a,b,c\n1"), [{"a": 1, "b": None, "c": None}])


class TestTypes(unittest.TestCase):
    def test_ints_and_negatives(self):
        self.assertEqual(parse("a,b\n42,-7"), [{"a": 42, "b": -7}])

    def test_not_quite_ints_stay_strings(self):
        self.assertEqual(parse("a,b,c\n1.5,+3,1a"),
                         [{"a": "1.5", "b": "+3", "c": "1a"}])

    def test_quoted_digits_stay_a_string(self):
        self.assertEqual(parse('a\n"007"'), [{"a": "007"}])

    def test_unquoted_empty_is_none(self):
        self.assertEqual(parse("a,b\n,2"), [{"a": None, "b": 2}])

    def test_quoted_empty_is_the_empty_string(self):
        self.assertEqual(parse('a,b\n"",2'), [{"a": "", "b": 2}])


class TestHeaderErrors(unittest.TestCase):
    def test_duplicate_header(self):
        with self.assertRaises(ParseError):
            parse("a,a\n1,2")

    def test_empty_header_name(self):
        with self.assertRaises(ParseError):
            parse("a,,b\n1,2,3")

    def test_parse_error_is_an_exception(self):
        self.assertTrue(issubclass(ParseError, Exception))


if __name__ == "__main__":
    unittest.main()
