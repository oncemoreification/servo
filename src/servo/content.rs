#[doc="
    The content task is the main task that runs JavaScript and spawns layout
    tasks.
"]

export ControlMsg, PingMsg;
export content;

import dom::base::NodeScope;
import dom::rcu::WriterMethods;
import dom::style;
import parser::lexer::{spawn_css_lexer_task, spawn_html_parser_task};
import parser::css_builder::build_stylesheet;
import parser::html_builder::build_dom;
import layout::layout_task;

import js::rust::methods;

import result::extensions;

enum ControlMsg {
    ParseMsg(~str),
    ExecuteMsg(~str),
    ExitMsg
}

enum PingMsg {
    PongMsg
}

#[doc="Sends a ping to layout and waits for the response."]
#[warn(no_non_implicitly_copyable_typarams)]
fn join_layout(scope: NodeScope, to_layout: chan<layout_task::Msg>) {

    if scope.is_reader_forked() {
        comm::listen {
            |from_layout|
            to_layout.send(layout_task::PingMsg(from_layout));
            from_layout.recv();
        }
        scope.reader_joined();
    }
}

#[warn(no_non_implicitly_copyable_typarams)]
fn content(to_layout: chan<layout_task::Msg>) -> chan<ControlMsg> {
    task::spawn_listener::<ControlMsg> {
        |from_master|
        let scope = NodeScope();
        let rt = js::rust::rt();
        loop {
            alt from_master.recv() {
              ParseMsg(filename) {
                #debug["content: Received filename `%s` to parse", *filename];

                // TODO actually parse where the css sheet should be
                // Replace .html with .css and try to open a stylesheet
                assert (*filename).ends_with(".html");
                let new_file = (*filename).substr(0u, (*filename).len() - 5u) + ".css";

                // Send off a task to parse the stylesheet
                let css_port = comm::port();
                let css_chan = comm::chan(css_port);
                task::spawn {||
                    let new_file <- new_file;
                    let css_stream = spawn_css_lexer_task(~new_file);
                    let css_rules = build_stylesheet(css_stream);
                    css_chan.send(css_rules);
                };

                // Note: we can parse the next document in parallel
                // with any previous documents.
                let stream = spawn_html_parser_task(filename);
                let root = build_dom(scope, stream);
           
                // Collect the css stylesheet
                let css_rules = comm::recv(css_port);
                
                // Apply the css rules to the dom tree:
                // TODO
                #debug["%s",style::print_sheet(css_rules)];
                
               
                // Now, join the layout so that they will see the latest
                // changes we have made.
                join_layout(scope, to_layout);

                // Send new document to layout.
                to_layout.send(layout_task::BuildMsg(root, css_rules));

                // Indicate that reader was forked so any further
                // changes will be isolated.
                scope.reader_forked();
              }

              ExecuteMsg(filename) {
                #debug["content: Received filename `%s` to execute", *filename];

                alt io::read_whole_file(*filename) {
                  result::err(msg) {
                    io::println(#fmt["Error opening %s: %s", *filename, msg]);
                  }
                  result::ok(bytes) {
                    let cx = rt.cx();
                    cx.set_default_options_and_version();
                    cx.set_logging_error_reporter();
                    cx.new_compartment(js::global::global_class).chain {
                        |compartment|
                        compartment.define_functions(js::global::debug_fns);
                        cx.evaluate_script(compartment.global_obj, bytes, *filename, 1u)
                    };
                  }
                }
              }

              ExitMsg {
                to_layout.send(layout_task::ExitMsg);
                break;
              }
            }
        }
    }
}
