use super::editor::{BumpOption, Focus, Input, Mode, PlanEditor, Row};
use crate::bump::BumpRule;
use crate::plan::Reason;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

pub fn render(frame: &mut Frame, editor: &PlanEditor, title: &str) {
  let [header, body, footer] = Layout::vertical([
    Constraint::Length(1),
    Constraint::Min(0),
    Constraint::Length(1),
  ])
  .areas(frame.area());

  let releasing = editor.plan().packages.len();
  let summary = format!(" · {releasing} of {} packages", editor.rows.len());
  frame.render_widget(Line::from(vec![title.bold(), summary.dim()]), header);
  match &editor.mode {
    Mode::Review { lines, scroll } => render_review(frame, body, lines, *scroll),
    _ => {
      let [left, right] =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(body);
      render_packages(frame, left, editor);
      render_details(frame, right, editor);
      if let Mode::Bump { options, cursor } = &editor.mode {
        render_bump(frame, right, editor, options, *cursor);
      }
    }
  }
  frame.render_widget(footer_line(editor), footer);
}

fn focus_style(focused: bool) -> Style {
  match focused {
    true => Style::new().cyan(),
    false => Style::new().dark_gray(),
  }
}

fn rule_label(rule: &BumpRule) -> String {
  match rule {
    BumpRule::Exact(version) => format!("exact {version}"),
    rule => rule.name().to_string(),
  }
}

fn render_packages(frame: &mut Frame, area: Rect, editor: &PlanEditor) {
  let name_width = editor
    .rows
    .iter()
    .map(|package| package.name().len())
    .max()
    .unwrap_or_default();
  let items = editor.rows.iter().map(|package| {
    let current = package.version();
    let row = editor.row(package);
    let (mark, next, rule, style) = match &row {
      Row::Released(planned) => (
        "[x]",
        planned.next_version().to_string(),
        rule_label(&planned.rule),
        Style::new(),
      ),
      Row::Skipped => ("[ ]", "-".into(), "skip".into(), Style::new().dim()),
      Row::LeftOut => ("[ ]", "-".into(), "left out".into(), Style::new().dim()),
    };
    let name = package.name();
    let mut lines = vec![Line::styled(
      format!("{mark} {name:name_width$}  {current} → {next}  {rule}"),
      style,
    )];
    if let Row::Released(planned) = row {
      let because = match planned.reason {
        Reason::Changed => None,
        Reason::Fixed => Some("fixed group".to_string()),
        Reason::Dependency => {
          let dependencies = editor.plan().released_dependencies(planned);
          let names = dependencies
            .iter()
            .map(|dependency| dependency.package.name());
          Some(format!("deps: {}", names.collect::<Vec<_>>().join(", ")))
        }
      };
      if let Some(because) = because {
        lines.push(Line::from(format!("    └ {because}")).cyan());
      }
    }
    ListItem::new(lines)
  });

  let block = Block::bordered()
    .title(" Packages ")
    .border_style(focus_style(editor.focus == Focus::Packages));
  let list = List::new(items)
    .block(block)
    .highlight_style(Style::new().reversed());
  let mut state = ListState::default().with_selected(Some(editor.cursor));
  frame.render_stateful_widget(list, area, &mut state);
}

fn render_details(frame: &mut Frame, area: Rect, editor: &PlanEditor) {
  let package = editor.package();
  let row = editor.row(package);
  let title = match &row {
    Row::Released(planned) => format!(
      " {} {} → {} ",
      package.name(),
      package.version(),
      planned.next_version()
    ),
    _ => format!(" {} {} ", package.name(), package.version()),
  };
  let block = Block::bordered()
    .title(title)
    .border_style(focus_style(editor.focus == Focus::Changes));
  let inner = block.inner(area);
  frame.render_widget(block, area);
  let [bump, changes, preview] = Layout::vertical([
    Constraint::Length(2),
    Constraint::Percentage(45),
    Constraint::Min(0),
  ])
  .areas(inner);

  let bump_line = match &row {
    Row::Released(planned) => {
      let source = match editor.is_pinned(package) {
        true => "chosen",
        false => "from changes",
      };
      Line::from(vec![
        "Bump  ".bold(),
        rule_label(&planned.rule).yellow(),
        format!("  ({source})").dim(),
      ])
    }
    Row::Skipped => Line::from("Skipped").dim(),
    Row::LeftOut => Line::from("Left out: every package it follows is skipped").dim(),
  };
  frame.render_widget(bump_line, bump);

  let items = editor.changes();
  let changes_block = Block::new().borders(Borders::TOP).title("Changes");
  if items.is_empty() {
    let empty = Paragraph::new("No changes of its own".dim()).block(changes_block);
    frame.render_widget(empty, changes);
  } else {
    let list = List::new(items.iter().map(|item| {
      let mark = if item.included { "[x]" } else { "[ ]" };
      let line = Line::from(format!("{mark} {}", item.label));
      ListItem::new(if item.included { line } else { line.dim() })
    }))
    .block(changes_block)
    .highlight_style(Style::new().reversed());
    let selected = (editor.focus == Focus::Changes).then_some(editor.change_cursor);
    let mut state = ListState::default().with_selected(selected);
    frame.render_stateful_widget(list, changes, &mut state);
  }

  let preview_block = Block::new().borders(Borders::TOP).title("Changelog");
  let text = editor.changelog_preview().unwrap_or_default();
  let paragraph = Paragraph::new(text)
    .block(preview_block)
    .wrap(Wrap { trim: false });
  frame.render_widget(paragraph, preview);
}

fn render_bump(
  frame: &mut Frame,
  area: Rect,
  editor: &PlanEditor,
  options: &[BumpOption],
  cursor: usize,
) {
  let items = options.iter().map(|option| {
    let ellipsis = if option.pick.asks() { "…" } else { "" };
    let label = format!("{}{ellipsis}", option.pick.label());
    let preview = option
      .preview
      .as_ref()
      .map_or(String::new(), ToString::to_string);
    let mut spans = vec![Span::from(format!(" {label:12}")), preview.yellow()];
    if option.current {
      spans.push("  current".dim());
    }
    ListItem::new(Line::from(spans))
  });
  let package = editor.package();
  let block = Block::bordered()
    .title(format!(" Bump {} {} ", package.name(), package.version()))
    .border_style(Style::new().cyan());
  let popup = Rect {
    x: area.x + 2,
    y: area.y + 1,
    width: area.width.saturating_sub(4).min(44),
    height: (options.len() as u16 + 2).min(area.height.saturating_sub(1)),
  };
  let list = List::new(items)
    .block(block)
    .highlight_style(Style::new().reversed());
  let mut state = ListState::default().with_selected(Some(cursor));
  frame.render_widget(Clear, popup);
  frame.render_stateful_widget(list, popup, &mut state);
}

fn render_review(frame: &mut Frame, area: Rect, lines: &[String], scroll: u16) {
  let text = Text::from(
    lines
      .iter()
      .map(|line| Line::from(line.as_str()))
      .collect::<Vec<_>>(),
  );
  let paragraph = Paragraph::new(text)
    .block(Block::bordered().title(" Review "))
    .scroll((scroll, 0));
  frame.render_widget(paragraph, area);
}

fn footer_line<'e>(editor: &'e PlanEditor) -> Line<'e> {
  let hint = |text: &'static str| Span::from(text).dim();
  let message = editor.message.as_deref().map(|message| message.yellow());
  match &editor.mode {
    Mode::Input { kind, text } => {
      let package = editor.package();
      let prompt = match kind {
        Input::Version => format!(
          "Version of {} (now {}): ",
          package.name(),
          package.version()
        ),
        Input::Prerelease(pre) => {
          let rule = pre.rule(Default::default()).name();
          format!("Prerelease id for {} ({rule}): ", package.name())
        }
      };
      let mut spans = vec![prompt.bold(), Span::from(text.as_str()), "▏".into()];
      match message {
        Some(message) => spans.extend(["  ".into(), message]),
        None => spans.push(hint("  enter ok · esc cancel")),
      }
      Line::from(spans)
    }
    _ if message.is_some() => Line::from(message.unwrap_or_default()),
    Mode::Browse => Line::from(hint(
      "↑↓ move  tab switch  space include  b bump  v version  enter review  q quit",
    )),
    Mode::Bump { .. } => Line::from(hint("↑↓ choose  enter select  esc cancel")),
    Mode::Review { .. } => Line::from(vec![
      hint("↑↓ scroll  "),
      " y apply ".bold().black().on_cyan(),
      hint("  esc back  q quit"),
    ]),
  }
}
