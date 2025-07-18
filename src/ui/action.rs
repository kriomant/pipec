use crossterm::event::KeyEvent;

#[derive(Clone)]
pub(crate) enum QuitAction {
    Succeed,
    Cancel,
    PrintPipeline,
    PrintOutput,
}

#[derive(Clone)]
pub(crate) enum CopySource {
    FocusedStageCommand,
    WholePipeline,
    ShownStageOutput,
    FinalStageOuput,
}

#[derive(Clone)]
pub(crate) enum Action {
    Quit(QuitAction),
    CopyToClipboard(CopySource),
    Execute,
    ExecuteAll,
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
    RestartPipeline,
}
