codeunit 50101 "Test Event Publisher"
{
    [IntegrationEvent(false, false)]
    procedure OnBeforeProcess(var InputValue: Text; var IsHandled: Boolean)
    begin
    end;

    [IntegrationEvent(false, false)]
    procedure OnAfterProcess(InputValue: Text; Result: Boolean)
    begin
    end;

    procedure DoProcess(InputValue: Text): Boolean
    var
        IsHandled: Boolean;
        Success: Boolean;
    begin
        IsHandled := false;
        OnBeforeProcess(InputValue, IsHandled);
        if IsHandled then
            exit(true);

        Success := InputValue <> '';
        OnAfterProcess(InputValue, Success);
        exit(Success);
    end;
}
