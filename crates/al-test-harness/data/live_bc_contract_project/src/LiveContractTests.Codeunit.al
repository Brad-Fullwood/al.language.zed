codeunit 50100 "Live Contract Tests"
{
    Subtype = Test;

    [Test]
    procedure PublishDebugAndSnapshot()
    var
        ObservedValue: Integer;
        Payload: JsonObject;
    begin
        ObservedValue := 40;
        ObservedValue += 2;
        Payload.ReadFrom('{}'); // LIVE_BC_BREAKPOINT
        if ObservedValue <> 42 then
            Error('Expected 42, got %1.', ObservedValue);
    end;
}
