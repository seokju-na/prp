use crate::bump::BumpRule;
use crate::changelog;
use crate::changes::ChangeId;
use crate::package::Package;
use crate::plan::{Decisions, PackagePlan, Plan, Planner};
use crate::registry::Registries;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use semver::{Prerelease, Version};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
  Packages,
  Changes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
  Browse,
  Bump {
    options: Vec<BumpOption>,
    cursor: usize,
  },
  Input {
    kind: Input,
    text: String,
  },
  Review {
    lines: Vec<String>,
    scroll: u16,
  },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
  Version,
  Prerelease(PreRule),
}

/// A prerelease rule still waiting for its identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreRule {
  Prepatch,
  Preminor,
  Premajor,
  Prerelease,
}

impl PreRule {
  pub fn rule(self, pre: Prerelease) -> BumpRule {
    match self {
      Self::Prepatch => BumpRule::Prepatch(pre),
      Self::Preminor => BumpRule::Preminor(pre),
      Self::Premajor => BumpRule::Premajor(pre),
      Self::Prerelease => BumpRule::Prerelease(pre),
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
  Auto,
  Rule(BumpRule),
  Pre(PreRule),
  Exact,
}

impl Pick {
  pub fn label(&self) -> &'static str {
    match self {
      Self::Auto => "auto",
      Self::Rule(rule) => rule.name(),
      Self::Pre(pre) => pre.rule(Prerelease::EMPTY).name(),
      Self::Exact => "exact",
    }
  }

  /// Whether choosing it asks for more input.
  pub fn asks(&self) -> bool {
    matches!(self, Self::Pre(_) | Self::Exact)
  }

  fn matches(&self, current: Option<&BumpRule>) -> bool {
    match (self, current) {
      (Self::Auto, None) => true,
      (Self::Exact, Some(BumpRule::Exact(_))) => true,
      (Self::Rule(rule), Some(current)) => rule == current,
      (Self::Pre(pre), Some(current)) => pre.rule(Prerelease::EMPTY).name() == current.name(),
      _ => false,
    }
  }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BumpOption {
  pub pick: Pick,
  /// The version it leads to, with the default identifier for prereleases.
  pub preview: Option<Version>,
  pub current: bool,
}

/// What the event loop has to do after a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
  None,
  Review,
  Apply,
  Quit,
}

pub enum Row<'p, 'a> {
  Released(&'p PackagePlan<'a>),
  Skipped,
  /// Not skipped, but everything it followed is.
  LeftOut,
}

pub struct ChangeItem {
  pub id: ChangeId,
  pub label: String,
  pub included: bool,
}

/// Edits the decisions about a release, keeping the plan built from them current.
pub struct PlanEditor<'a> {
  planner: &'a Planner<'a>,
  registries: &'a Registries,
  decisions: Decisions<'a>,
  plan: Plan<'a>,
  pub(super) rows: Vec<&'a Package>,
  pub(super) cursor: usize,
  pub(super) change_cursor: usize,
  pub(super) focus: Focus,
  pub(super) mode: Mode,
  pub(super) message: Option<String>,
}

impl<'a> PlanEditor<'a> {
  /// `plan` is the one planned without decisions; its packages are the rows.
  pub fn new(planner: &'a Planner<'a>, plan: Plan<'a>, registries: &'a Registries) -> Self {
    Self {
      planner,
      registries,
      decisions: Decisions::default(),
      rows: plan
        .packages
        .iter()
        .map(|planned| planned.package)
        .collect(),
      plan,
      cursor: 0,
      change_cursor: 0,
      focus: Focus::Packages,
      mode: Mode::Browse,
      message: None,
    }
  }

  pub fn plan(&self) -> &Plan<'a> {
    &self.plan
  }

  pub fn into_plan(self) -> Plan<'a> {
    self.plan
  }

  pub fn review(&mut self, lines: Vec<String>) {
    self.mode = Mode::Review { lines, scroll: 0 };
  }

  pub fn package(&self) -> &'a Package {
    self.rows[self.cursor]
  }

  pub fn row(&self, package: &Package) -> Row<'_, 'a> {
    match self.plan.get(package) {
      Some(planned) => Row::Released(planned),
      None if self.is_skipped(package) => Row::Skipped,
      None => Row::LeftOut,
    }
  }

  pub fn is_pinned(&self, package: &Package) -> bool {
    self
      .decisions
      .get(package)
      .is_some_and(|decision| decision.rule.is_some())
  }

  /// Every change detected for the current package.
  pub fn changes(&self) -> Vec<ChangeItem> {
    let package = self.package();
    let excluded = self
      .decisions
      .get(package)
      .map(|decision| &decision.excluded);
    let detected = self.planner.changes_of(package);
    let notes = detected
      .notes
      .iter()
      .map(|note| (note.id(), format!("* {}", note.title)));
    let commits = detected.commits.iter().map(|commit| {
      let id = ChangeId::Commit(commit.sha.clone());
      (id, format!("{} {}", &commit.sha[..7], commit.summary))
    });
    notes
      .chain(commits)
      .map(|(id, label)| ChangeItem {
        included: !excluded.is_some_and(|excluded| excluded.contains(&id)),
        id,
        label,
      })
      .collect()
  }

  /// The changelog section of the current package, as it would be written.
  pub fn changelog_preview(&self) -> Option<String> {
    let package = self.package();
    let config = &self.planner.workspace().config.changelog;
    let section = package.changelog_section(&self.plan, config, self.registries)?;
    let heading = changelog::package_heading(self.plan.get(package)?.next_version());
    Some(format!("{heading}\n\n{section}"))
  }

  pub fn handle(&mut self, key: KeyEvent) -> Effect {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
      return Effect::Quit;
    }
    self.message = None;
    match self.mode {
      Mode::Browse => return self.browse(key.code),
      Mode::Bump { .. } => self.picking(key.code),
      Mode::Input { .. } => self.typing(key.code),
      Mode::Review { .. } => return self.reviewing(key.code),
    }
    Effect::None
  }

  fn browse(&mut self, code: KeyCode) -> Effect {
    match code {
      KeyCode::Up | KeyCode::Char('k') => self.move_cursor(-1),
      KeyCode::Down | KeyCode::Char('j') => self.move_cursor(1),
      KeyCode::Tab => self.focus(match self.focus {
        Focus::Packages => Focus::Changes,
        Focus::Changes => Focus::Packages,
      }),
      KeyCode::Left | KeyCode::Char('h') => self.focus(Focus::Packages),
      KeyCode::Right | KeyCode::Char('l') => self.focus(Focus::Changes),
      KeyCode::Char(' ') => match self.focus {
        Focus::Packages => self.toggle_skip(),
        Focus::Changes => self.toggle_change(),
      },
      KeyCode::Char('b') => self.open_bump(),
      KeyCode::Char('v') => self.start_version(),
      KeyCode::Enter if self.plan.is_empty() => {
        self.message = Some("Nothing is selected to release".into());
      }
      KeyCode::Enter => return Effect::Review,
      KeyCode::Char('q') | KeyCode::Esc => return Effect::Quit,
      _ => {}
    }
    Effect::None
  }

  fn reviewing(&mut self, code: KeyCode) -> Effect {
    let Mode::Review { lines, scroll } = &mut self.mode else {
      return Effect::None;
    };
    let last = lines.len().saturating_sub(1) as u16;
    match code {
      KeyCode::Char('y') => return Effect::Apply,
      KeyCode::Char('q') => return Effect::Quit,
      KeyCode::Esc | KeyCode::Char('n') | KeyCode::Backspace => self.mode = Mode::Browse,
      KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
      KeyCode::Down | KeyCode::Char('j') => *scroll = (*scroll + 1).min(last),
      KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
      KeyCode::PageDown => *scroll = (*scroll + 10).min(last),
      _ => {}
    }
    Effect::None
  }

  fn picking(&mut self, code: KeyCode) {
    let Mode::Bump { options, cursor } = &mut self.mode else {
      return;
    };
    match code {
      KeyCode::Up | KeyCode::Char('k') => *cursor = cursor.saturating_sub(1),
      KeyCode::Down | KeyCode::Char('j') => *cursor = (*cursor + 1).min(options.len() - 1),
      KeyCode::Esc | KeyCode::Char('b') | KeyCode::Char('q') => self.mode = Mode::Browse,
      KeyCode::Enter => {
        let pick = options[*cursor].pick.clone();
        self.choose(pick);
      }
      _ => {}
    }
  }

  fn choose(&mut self, pick: Pick) {
    let rule = match pick {
      Pick::Auto => None,
      Pick::Rule(rule) => Some(rule),
      Pick::Pre(pre) => {
        let text = self.prerelease_id().to_string();
        self.mode = Mode::Input {
          kind: Input::Prerelease(pre),
          text,
        };
        return;
      }
      Pick::Exact => return self.start_version(),
    };
    if self.pin(rule) {
      self.mode = Mode::Browse;
    }
  }

  fn typing(&mut self, code: KeyCode) {
    let Mode::Input { kind, text } = &mut self.mode else {
      return;
    };
    match code {
      KeyCode::Char(c) => text.push(c),
      KeyCode::Backspace => {
        text.pop();
      }
      KeyCode::Esc => self.mode = Mode::Browse,
      KeyCode::Enter => {
        let (kind, text) = (*kind, text.trim().to_string());
        if self.submit(kind, &text) {
          self.mode = Mode::Browse;
        }
      }
      _ => {}
    }
  }

  fn submit(&mut self, kind: Input, text: &str) -> bool {
    let rule = match kind {
      Input::Version => Version::parse(text)
        .map(BumpRule::Exact)
        .map_err(|_| format!("\"{text}\" is not a version")),
      Input::Prerelease(pre) => Prerelease::new(text)
        .map(|id| pre.rule(id))
        .map_err(|_| format!("\"{text}\" is not a prerelease identifier")),
    };
    match rule {
      Ok(rule) => self.pin(Some(rule)),
      Err(message) => {
        self.message = Some(message);
        false
      }
    }
  }

  fn move_cursor(&mut self, delta: isize) {
    let step = |cursor: usize, len: usize| {
      cursor
        .saturating_add_signed(delta)
        .min(len.saturating_sub(1))
    };
    match self.focus {
      Focus::Packages => {
        self.cursor = step(self.cursor, self.rows.len());
        self.change_cursor = 0;
      }
      Focus::Changes => self.change_cursor = step(self.change_cursor, self.changes().len()),
    }
  }

  fn focus(&mut self, focus: Focus) {
    if focus == Focus::Changes && self.changes().is_empty() {
      self.message = Some(format!(
        "{} has no changes of its own",
        self.package().name()
      ));
      return;
    }
    self.focus = focus;
  }

  fn toggle_skip(&mut self) {
    let package = self.package();
    if let Row::LeftOut = self.row(package) {
      self.message = Some(format!(
        "{} is left out with the packages it follows",
        package.name()
      ));
      return;
    }
    let skip = !self.is_skipped(package);
    let members = self.planner.fixed_with(package);
    self.update(|decisions| {
      for member in members {
        decisions.edit(member).skip = skip;
      }
    });
  }

  fn toggle_change(&mut self) {
    let package = self.package();
    let Some(item) = self.changes().into_iter().nth(self.change_cursor) else {
      return;
    };
    self.update(|decisions| {
      let excluded = &mut decisions.edit(package).excluded;
      if !excluded.remove(&item.id) {
        excluded.insert(item.id);
      }
    });
  }

  fn open_bump(&mut self) {
    if !self.ensure_released() {
      return;
    }
    let version = self.package().version();
    let mut picks = vec![
      Pick::Auto,
      Pick::Rule(BumpRule::Patch),
      Pick::Rule(BumpRule::Minor),
      Pick::Rule(BumpRule::Major),
      Pick::Pre(PreRule::Prepatch),
      Pick::Pre(PreRule::Preminor),
      Pick::Pre(PreRule::Premajor),
    ];
    if !version.pre.is_empty() {
      picks.push(Pick::Pre(PreRule::Prerelease));
      picks.push(Pick::Rule(BumpRule::Release));
    }
    picks.push(Pick::Exact);

    let current = self.pinned_rule();
    let id = self.prerelease_id();
    let options = picks
      .into_iter()
      .map(|pick| BumpOption {
        preview: self.preview(&pick, &id),
        current: pick.matches(current.as_ref()),
        pick,
      })
      .collect::<Vec<_>>();
    let cursor = options
      .iter()
      .position(|option| option.current)
      .unwrap_or(0);
    self.mode = Mode::Bump { options, cursor };
  }

  fn preview(&self, pick: &Pick, id: &Prerelease) -> Option<Version> {
    let rule = match pick {
      Pick::Auto => None,
      Pick::Rule(rule) => Some(rule.clone()),
      Pick::Pre(pre) => Some(pre.rule(id.clone())),
      Pick::Exact => return None,
    };
    let plan = self.planner.plan(&self.with_rule(rule)).ok()?;
    Some(plan.get(self.package())?.next_version().clone())
  }

  fn pinned_rule(&self) -> Option<BumpRule> {
    self
      .decisions
      .get(self.package())
      .and_then(|decision| decision.rule.clone())
  }

  /// The identifier last chosen, else the current series restarted, else `beta.0`.
  fn prerelease_id(&self) -> Prerelease {
    if let Some(pre) = self.pinned_rule().as_ref().and_then(BumpRule::prerelease) {
      return pre.clone();
    }
    let current = &self.package().version().pre;
    let series = current
      .split('.')
      .next()
      .filter(|id| !id.is_empty() && id.parse::<u64>().is_err())
      .unwrap_or("beta");
    Prerelease::new(&format!("{series}.0")).unwrap_or(Prerelease::EMPTY)
  }

  fn start_version(&mut self) {
    if !self.ensure_released() {
      return;
    }
    let next = self.plan.get(self.package()).map(PackagePlan::next_version);
    self.mode = Mode::Input {
      kind: Input::Version,
      text: next.map(ToString::to_string).unwrap_or_default(),
    };
  }

  fn ensure_released(&mut self) -> bool {
    let released = matches!(self.row(self.package()), Row::Released(_));
    if !released {
      self.message = Some(format!("{} is not released", self.package().name()));
    }
    released
  }

  fn pin(&mut self, rule: Option<BumpRule>) -> bool {
    self.apply(self.with_rule(rule))
  }

  /// Fixed groups share one version, so the rule goes to every member.
  fn with_rule(&self, rule: Option<BumpRule>) -> Decisions<'a> {
    let mut decisions = self.decisions.clone();
    for member in self.planner.fixed_with(self.package()) {
      decisions.edit(member).rule = rule.clone();
    }
    decisions
  }

  fn update(&mut self, edit: impl FnOnce(&mut Decisions<'a>)) -> bool {
    let mut decisions = self.decisions.clone();
    edit(&mut decisions);
    self.apply(decisions)
  }

  /// Takes `decisions` only if a plan can be made from them.
  fn apply(&mut self, decisions: Decisions<'a>) -> bool {
    match self.planner.plan(&decisions) {
      Ok(plan) => {
        self.decisions = decisions;
        self.plan = plan;
        true
      }
      Err(error) => {
        self.message = Some(format!("{error:#}"));
        false
      }
    }
  }

  fn is_skipped(&self, package: &Package) -> bool {
    self
      .decisions
      .get(package)
      .is_some_and(|decision| decision.skip)
  }
}
