use crossterm::event::KeyEvent;

#[derive(Clone)]
pub(crate) enum QuitAction {
    Succeed,
    Cancel,
    PrintPipeline,
    PrintOutput,
}

#[derive(Clone)]
pub(crate) enum Action {
    Quit(QuitAction),
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
