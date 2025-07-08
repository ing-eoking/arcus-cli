use std::rc::Rc;
use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::cell::RefCell;

#[derive(Default)]
struct Node {
    pub token: String,
    pub skip: bool,
    nread: bool,
    anything: bool,
    children: HashSet<Rc<RefCell<Node>>>,
}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.token == other.token
    }
}

impl Eq for Node {}

impl Hash for Node {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.token.hash(state);
    }
}

impl Node {
    fn add_child(&mut self, child: Rc<Node>) {
        self.children.insert(child);
    }
}

pub struct Tree {
    pub root: Node
}

impl Tree {
    fn insert(&mut self, input: String) {
        let mut lines = input.lines();
        let mut _vstr: Vec<_> = lines.next().unwrap_or("").split_whitespace().collect();
        for line in lines {
            _vstr.push(line);
        }
    }
    fn initialize(&mut self) {
        Node { token: "get", false, false, false, HashSet<Rc<}
    }
}

fn main() {
    let input = "hello world from rust\nthis is the second line\nthird line here";

    let mut lines = input.lines();

    if let Some(first_line) = lines.next() {
        // 첫 줄은 띄어쓰기 기준으로 split
        let first_split: Vec<&str> = first_line.split_whitespace().collect();
        println!("First line split: {:?}", first_split);
    }

    // 나머지 줄은 그대로 출력
    for line in lines {
        println!("Remaining line: {}", line);
    }
}
