use crossterm::event::KeyEvent;

pub(crate) enum Action {
    Quit,
    Execute,
    FocusPreviousStage,
    FocusNextStage,
    ShowStageOutput,
    NewStageAbove,
    NewStageBelow,
    DeleteStage,
    ToggleStage,
    AbortPipeline,
    LaunchPager,
    LaunchEditor,
    Input(KeyEvent),
}
