// Overlay file for eval case 03 (bc-event-map). The bundled fixture at
// crates/al-test-harness/data/test_al_project publishes OnBeforeProcess and
// OnAfterProcess (see src/CodeunitWithEvents.al) but ships no subscriber, so
// there is nothing for a "who subscribes to X" question to find. run.sh
// copies this file into a scratch copy of the fixture before running the
// subscriber case, matching how plugin/TESTING.md's question 3 was tested by
// hand.
codeunit 50132 "Work Order Subscribers"
{
    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Test Event Publisher", 'OnBeforeProcess', '', false, false)]
    local procedure HandleOnBeforeProcess(var InputValue: Text; var IsHandled: Boolean)
    begin
    end;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::"Test Event Publisher", 'OnAfterProcess', '', false, false)]
    local procedure HandleOnAfterProcess(InputValue: Text; Result: Boolean)
    begin
    end;
}
