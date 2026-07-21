codeunit 50131 "Work Order Post Task"
{
    TableNo = "Work Order Staging";

    trigger OnRun()
    begin
        Run(Rec);
    end;

    procedure Run(var Staging: Record "Work Order Staging")
    var
        ErrorText: Text;
    begin
        if Staging.Status <> Staging.Status::Posted then
            exit;

        Staging.SetRange(Status, Staging.Status::Posted);
        if not InsertJournalLine(Staging, ErrorText, 0) then
            MarkStagingFailed(Staging, ErrorText);
    end;

    procedure InsertJournalLine(var Staging: Record "Work Order Staging"; var ErrorText: Text; PostedEntryNo: Integer): Boolean
    var
        LocalCounter: Integer;
    begin
        LocalCounter := PostedEntryNo + 1;
        if Staging.Amount = 0 then begin
            ErrorText := 'Amount must not be zero';
            exit(false);
        end;
        exit(true);
    end;

    procedure MarkStagingFailed(var Staging: Record "Work Order Staging"; ErrorText: Text)
    begin
        Staging.Status := Staging.Status::Failed;
        ErrorText := GetLastErrorText();
        Staging.Modify();
    end;

    procedure ScheduleBackgroundTask(var Staging: Record "Work Order Staging")
    var
        TaskId: Guid;
    begin
        TaskId := TaskScheduler.CreateTask(
            Codeunit::"Work Order Post Task",
            0,
            true,
            CompanyName(),
            CurrentDateTime(),
            Staging.RecordId);
    end;
}
