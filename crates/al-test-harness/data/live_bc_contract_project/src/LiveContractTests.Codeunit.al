codeunit 50100 "Live Contract Tests"
{
    Subtype = Test;

    [Test]
    procedure PublishDebugAndSnapshot()
    var
        ObservedValue: Integer;
        Client: HttpClient;
    begin
        ObservedValue := 40;
        ObservedValue += 2;
        Client.Clear(); // LIVE_BC_BREAKPOINT
        if ObservedValue <> 42 then
            Error('Expected 42, got %1.', ObservedValue);
    end;
}
