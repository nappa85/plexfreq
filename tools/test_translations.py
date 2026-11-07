import unittest

from translations import core_messages, rust_code, validate


def contexts(text, name="src/x.rs"):
    return core_messages({name: text})


class ContextExtractionTests(unittest.TestCase):
    def test_last_literal_argument_of_each_context_function(self):
        code = '''
            fn f(&self) {
                self.request(Method::POST, url, "", &[("strong", "true")], "sign-in creation")?;
                bounded_json(response, "track metadata").await?;
                let v: Value = plex_json(&bytes, "lyrics")?;
                self.request(Method::GET, url, token, &[], context)?;
            }'''
        self.assertEqual(contexts(code), {"sign-in creation", "track metadata", "lyrics"})

    def test_comments_char_literals_and_raw_strings_do_not_desynchronise(self):
        code = '''
            // self.request(a, "commented out")  (unbalanced
            /* nested /* "block" */ ( */
            let quote = '"'; let paren = '(';
            let raw = r#"has "quotes" and ( brackets"#;
            fn g<'a>(x: &'a str) {}
            bounded_json(response, "after them")?;
            Error::ProtocolAt("direct")'''
        self.assertEqual(contexts(code), {"after them", "direct"})

    def test_test_modules_are_skipped_but_code_after_them_is_not(self):
        code = '''
            #[cfg(test)]
            mod tests { fn t() { plex_json(b"{}", "test"); let s = "}"; } }
            fn after() { plex_json(&bytes, "real") }'''
        self.assertEqual(contexts(code), {"real"})

    def test_a_new_context_wrapper_must_be_registered(self):
        code = """
            fn fetch(&self, form: &[(&str, &str)], what: &'static str) -> Result<()> {
                Err(Error::ProtocolAt(what))
            }"""
        with self.assertRaisesRegex(ValueError, "fetch passes a context"):
            contexts(code)
        contexts(code.replace("fn fetch", "fn plex_json"))

    def test_input_messages_in_match_arms_and_constants(self):
        code = """
            const GONE: &str = "Track is gone";
            const CANCELLED_MESSAGE: &str = "Request cancelled by navigation";
            fn f() {
                Err(Error::Input(match kind { A => "Album text", _ => "Track text" }));
                Err(crate::Error::Input(GONE));
                Err(Error::Input(CANCELLED_MESSAGE));
                if let Err(Error::Input(message)) = result {}
            }"""
        self.assertEqual(contexts(code), {"Album text", "Track text", "Track is gone"})
        with self.assertRaisesRegex(ValueError, "unresolved"):
            contexts("fn f() { Error::Input(ELSEWHERE) }")

    def test_call_forms_constants_paths_and_escapes(self):
        code = r"""
            const C: &str = "constant context";
            fn f() {
                plex_json::<Value>(&b, "turbofish")?;
                client.request::<Foo>(Method::GET, url, "", &[], "other receiver")?;
                crate::plex::bounded_json(response, "by path").await?;
                Err(Error::ProtocolAt(C));
                Err(Error::Input(Self::C));
                Err(Error::Input("it\'s \u{e9} \
                    continued"));
                MyError::Input("not ours");
            }
            #[cfg(all(test, unix))]
            #[allow(unused)]
            mod tests { fn t() { Error::Input("test only"); } }"""
        self.assertEqual(contexts(code), {"turbofish", "other receiver", "by path", "constant context", "it's é continued"})
        with self.assertRaisesRegex(ValueError, "neither a literal"):
            contexts("fn plex_json() { Error::ProtocolAt(self.context) }")

    def test_unterminated_input_is_reported(self):
        for text in ('plex_json(&b, "x"', '"open', "/* open"):
            with self.subTest(text=text), self.assertRaises(ValueError):
                contexts(text)

    def test_rust_code_keeps_string_contents(self):
        self.assertEqual(rust_code('a("x // y") // z'), 'a("x // y") ')


class ValidationTests(unittest.TestCase):
    def test_rejects_keyword_lists_wrong_script_and_leftover_english(self):
        for language, source, translation in [
            ("bg", "%1 MiB audio cache used", "%1 · MiB · Звук"),
            ("hi", "Play", "Play"),
            ("de", "Shuffle", "Shuffle"),
            ("hi", "Rusty Plexus", "Rusty Plexus"),
        ]:
            with self.subTest(source=source), self.assertRaises(ValueError):
                validate(language, source, translation)

    def test_accepts_names_units_and_declared_loanwords(self):
        validate("hi", "%1 dB", "%1 dB")
        validate("zh_CN", "X-Plex-Token", "X-Plex-Token")
        validate("de", "Radio", "Radio", {"Radio"})
        validate("bg", "%1/%2 tracks · %3 MiB", "%1/%2 песни · %3 MiB")
        validate("el", "Session expired; start again", "Η σύνδεση έληξε· ξεκινήστε ξανά")


if __name__ == "__main__":
    unittest.main()
