/** Text layout. */

use layout::box::{TextBox, RenderBox, RenderBoxData, UnscannedTextBox};

use gfx::text::text_run::TextRun;
use gfx::util::range::Range;

pub struct TextBoxData {
    run: @TextRun,
    range: Range,
}

pub fn TextBoxData(run: @TextRun, range: &const Range) -> TextBoxData {
    TextBoxData {
        run: run,
        range: copy *range,
    }
}

pub fn adapt_textbox_with_range(box_data: &RenderBoxData, run: @TextRun, 
                                range: &const Range) -> @RenderBox {
    assert range.begin() < run.char_len();
    assert range.end() <= run.char_len();
    assert range.length() > 0;

    debug!("Creating textbox with span: (strlen=%u, off=%u, len=%u) of textrun: %s",
           run.char_len(), range.begin(), range.length(), run.text);
    let new_box_data = copy *box_data;
    let new_text_data = TextBoxData(run, range);
    let metrics = run.metrics_for_range(range);
    new_box_data.position.size = metrics.bounding_box.size;
    @TextBox(move new_box_data, move new_text_data)
}

pub trait UnscannedMethods {
    pure fn raw_text(&self) -> ~str;
}

impl RenderBox : UnscannedMethods {
    pure fn raw_text(&self) -> ~str {
        match self {
            &UnscannedTextBox(_, ref s) => copy *s,
            _ => fail!(~"unsupported operation: box.raw_text() on non-unscanned text box.")
        }
    }
}
