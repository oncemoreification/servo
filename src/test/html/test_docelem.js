debug("hi");
var elem = document.documentElement;
debug("document.documentElement: " + elem);
debug("Document: " + Document);
debug("Node: " + Node);
debug("Document instanceof Node: " + (Document instanceof Node));
debug("elem instanceof Node: " + (elem instanceof Node));
debug("elem instanceof Document: " + (elem instanceof Document));
debug("document instanceof Document: " + (document instanceof Document));
debug("document instanceof Node: " + (document instanceof Node));
debug("elem.tagName: " + elem.tagName);
debug("elem.firstChild: " + elem.firstChild);
debug("elem.firstChild.tagName: " + elem.firstChild.tagName);
debug("elem.firstChild.nextSibling: " + elem.firstChild.nextSibling);
debug("elem.firstChild.nextSibling.tagName: " + elem.firstChild.nextSibling.tagName);
