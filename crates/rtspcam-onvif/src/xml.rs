//! A tiny XML tree, enough to pick values out of SOAP replies. Namespaces are ignored: elements
//! and attributes are matched by their local names.

use quick_xml::Reader;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};

use crate::OnvifError;

#[derive(Debug, Default, Clone)]
pub(crate) struct Node {
    pub(crate) name: String,
    attrs: Vec<(String, String)>,
    text: String,
    pub(crate) children: Vec<Node>,
}

fn local(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn open(e: &BytesStart<'_>) -> Node {
    let attrs = e
        .attributes()
        .flatten()
        .map(|a| {
            let value = a
                .unescape_value()
                .map(|v| v.into_owned())
                .unwrap_or_default();
            (local(a.key.local_name().as_ref()), value)
        })
        .collect();
    Node {
        name: local(e.local_name().as_ref()),
        attrs,
        ..Node::default()
    }
}

impl Node {
    /// Parses a document and returns its root element.
    pub(crate) fn parse(xml: &str) -> Result<Node, OnvifError> {
        let bad = |why: String| OnvifError::Protocol(format!("unreadable XML reply: {why}"));
        let mut reader = Reader::from_str(xml);
        let mut stack: Vec<Node> = Vec::new();
        let mut root = None;
        loop {
            match reader.read_event().map_err(|e| bad(e.to_string()))? {
                Event::Start(e) => stack.push(open(&e)),
                Event::Empty(e) => {
                    let node = open(&e);
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(node),
                        None => root = Some(node),
                    }
                }
                Event::End(_) => {
                    let node = stack.pop().ok_or_else(|| bad("unbalanced tags".into()))?;
                    match stack.last_mut() {
                        Some(parent) => parent.children.push(node),
                        None => root = Some(node),
                    }
                }
                Event::Text(t) => {
                    if let Some(top) = stack.last_mut() {
                        top.text
                            .push_str(&t.xml_content().map_err(|e| bad(e.to_string()))?);
                    }
                }
                Event::CData(c) => {
                    if let Some(top) = stack.last_mut() {
                        top.text
                            .push_str(&c.decode().map_err(|e| bad(e.to_string()))?);
                    }
                }
                Event::GeneralRef(r) => {
                    if let Some(top) = stack.last_mut() {
                        if let Ok(Some(ch)) = r.resolve_char_ref() {
                            top.text.push(ch);
                        } else if let Ok(name) = r.decode()
                            && let Some(s) = resolve_predefined_entity(&name)
                        {
                            top.text.push_str(s);
                        }
                    }
                }
                Event::Eof => break,
                _ => {}
            }
        }
        if !stack.is_empty() {
            return Err(bad("the document ends early".into()));
        }
        root.ok_or_else(|| bad("no elements".into()))
    }

    /// The text directly inside this element, trimmed.
    pub(crate) fn text(&self) -> &str {
        self.text.trim()
    }

    pub(crate) fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub(crate) fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    /// Follows `names` down through first children.
    pub(crate) fn path(&self, names: &[&str]) -> Option<&Node> {
        names.iter().try_fold(self, |node, name| node.child(name))
    }

    /// The first descendant (depth first, this element included) with this name.
    pub(crate) fn find(&self, name: &str) -> Option<&Node> {
        if self.name == name {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(name))
    }

    /// Every descendant with this name, in document order, not looking inside matches.
    pub(crate) fn find_all<'a>(&'a self, name: &str, out: &mut Vec<&'a Node>) {
        if self.name == name {
            out.push(self);
            return;
        }
        for c in &self.children {
            c.find_all(name, out);
        }
    }

    /// Text of the first descendant called `name`, if non-empty.
    pub(crate) fn text_of(&self, name: &str) -> Option<&str> {
        self.find(name).map(Node::text).filter(|t| !t.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_names_text_attributes_and_entities() {
        let n = Node::parse(
            r#"<?xml version="1.0"?>
            <s:Envelope xmlns:s="x"><s:Body>
              <a:Item token="t&amp;1" xmlns:a="y"><a:Name>Main &lt;stream&gt; &#65;</a:Name><a:Empty/></a:Item>
              <![CDATA[raw]]>
            </s:Body></s:Envelope>"#,
        )
        .unwrap();
        assert_eq!(n.name, "Envelope");
        let item = n.path(&["Body", "Item"]).unwrap();
        assert_eq!(item.attr("token"), Some("t&1"));
        assert_eq!(item.text_of("Name"), Some("Main <stream> A"));
        assert!(item.text_of("Empty").is_none());
        assert!(n.find("Missing").is_none());
    }

    #[test]
    fn finds_all_without_nesting() {
        let n = Node::parse("<r><p><p/></p><q><p/></q></r>").unwrap();
        let mut found = Vec::new();
        n.find_all("p", &mut found);
        assert_eq!(found.len(), 2);
    }

    #[test]
    fn broken_documents_are_errors() {
        assert!(Node::parse("<a><b></a>").is_err());
        assert!(Node::parse("").is_err());
        assert!(Node::parse("<a>").is_err());
    }
}
