// Test: Bare Option type without inline members
codeunit 50104 "Option Bare Type Test"
{
    var
        FieldType: Option;
        Selection: Option;

    procedure DoWork(HeadlineName: Option; var HeadlineText: Text)
    begin
    end;

    procedure GetStep(): Option
    begin
        exit(0);
    end;

    procedure GetDirection() Result: Option
    begin
        Result := 0;
    end;
}
